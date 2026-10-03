
/// Coverage PDF generator for the Firefox-vs-PDFKit visual compare loop.
///
/// Usage: `cargo run --example coverage -- [pdf_out] [json_out]`
/// Defaults are `target/compare/coverage.pdf` and
/// `target/compare/coverage.json` (created if missing) so no binary
/// output ever lands in the repo. Every element gets a visible
/// `[EXXX]` label and a manifest entry with the same coordinates.
///
/// Layout: 14 Letter pages (612x792 pt), 100+ distinct numbered
/// elements covering text, encodings, CID, paths, clip,
/// transforms, colors, transparency, shadings, images, forms,
/// annotations, outlines, metadata, mixed filters, CropBox and
/// /Rotate (page 13 renders rotated via the view mapping).
fn main() {
  let pdf_out = std::env::args()
    .nth(1)
    .unwrap_or_else(|| String::from("target/compare/coverage.pdf"));
  let json_out = std::env::args()
    .nth(2)
    .unwrap_or_else(|| String::from("target/compare/coverage.json"));
  if let Some(parent) = std::path::Path::new(&pdf_out).parent() {
    if !parent.as_os_str().is_empty() {
      std::fs::create_dir_all(parent).unwrap();
    }
  }
  if let Some(parent) = std::path::Path::new(&json_out).parent() {
    if !parent.as_os_str().is_empty() {
      std::fs::create_dir_all(parent).unwrap();
    }
  }
  let mut gen = Gen::new();
  let pdf = gen.build();
  let manifest = gen.manifest_json();
  let count = gen.entries.len();
  std::fs::write(&pdf_out, &pdf).unwrap();
  std::fs::write(&json_out, manifest).unwrap();
  println!("wrote {pdf_out} ({} bytes) + {json_out} ({count} elements)", pdf.len());
  // Self-check: load it back with PDFKit and verify the structure.
  let doc = pdfkit::PdfDocument::load_bytes(pdf).expect("coverage file must parse");
  assert_eq!(doc.page_count(), 14, "coverage must have 14 pages");
  assert!(count >= 100, "need 100+ elements, got {count}");
  let mut texts = 0;
  let mut paths = 0;
  let mut grads = 0;
  let mut images = 0;
  let mut annots = 0;
  for i in 0..doc.page_count() {
    let page = doc.page(i).unwrap();
    annots += page.annotations.len();
    for item in &page.items {
      match item {
        pdfkit::PageItem::Text(_) => texts += 1,
        pdfkit::PageItem::Path(_) => paths += 1,
        pdfkit::PageItem::Gradient(_) => grads += 1,
        pdfkit::PageItem::Image(placed) => {
          images += 1;
          println!("page {i}: image {}x{}", placed.image.width, placed.image.height);
        }
        pdfkit::PageItem::Skipped(reason) => println!("page {i}: skipped {reason}"),
        _ => {}
      }
    }
  }
  println!("self-check: {texts} runs, {paths} paths, {grads} gradients, {images} images, {annots} annots, {} outlines",
    doc.outlines().len());
  assert!(texts > 100, "expected 100+ text runs, got {texts}");
  assert!(grads == 2, "expected 2 shadings, got {grads}");
  assert!(images == 6, "expected 6 images, got {images}");
  assert!(annots == 9, "expected 9 annotations, got {annots}");
  assert_eq!(doc.outlines().len(), 2);
  assert_eq!(doc.info().title.as_deref(), Some("PDFKit Coverage"));
  println!("self-check ok");
}

struct Entry {
  id: String,
  page: usize,
  rect: [f32; 4],
  kind: &'static str,
  /// Optional verdict override. `"text"` judges the crop by ink
  /// edges instead of pixels (for annotation/outline/info crops
  /// dominated by anchor text, which uses SF Pro on our side and
  /// DejaVu in the reference by design).
  verdict: Option<&'static str>,
  /// Optional ours-side crop rect (PDF points). Needed when our page
  /// frame differs from the reference: page 12 renders CropBox-sized
  /// while poppler rasters the full MediaBox.
  ours_rect: Option<[f32; 4]>,
}

struct Builder {
  objects: Vec<(u32, Vec<u8>)>,
}

impl Builder {
  fn new() -> Self {
    Self { objects: Vec::new() }
  }

  fn add(&mut self, num: u32, body: Vec<u8>) {
    self.objects.push((num, body));
  }

  fn raw(&mut self, num: u32, text: &str) {
    self.add(num, text.as_bytes().to_vec());
  }

  fn stream(&mut self, num: u32, dict: &str, data: Vec<u8>) {
    let mut body = dict.as_bytes().to_vec();
    body.extend_from_slice(format!(" /Length {}", data.len()).as_bytes());
    body.extend_from_slice(b" >>\nstream\n");
    body.extend_from_slice(&data);
    body.extend_from_slice(b"\nendstream");
    self.add(num, body);
  }

  fn finish(mut self, root: u32, info: u32) -> Vec<u8> {
    self.objects.sort_by_key(|(n, _)| *n);
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (num, body) in &self.objects {
      offsets.push((*num, pdf.len()));
      pdf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
      pdf.extend_from_slice(body);
      pdf.extend_from_slice(b"\nendobj\n");
    }
    let size = self.objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    let mut at = 1u32;
    for (num, off) in &offsets {
      while at < *num {
        pdf.extend_from_slice(b"0000000000 00000 f \n");
        at += 1;
      }
      pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
      at += 1;
    }
    while at < size {
      pdf.extend_from_slice(b"0000000000 00000 f \n");
      at += 1;
    }
    pdf.extend_from_slice(
      format!("trailer\n<< /Size {size} /Root {root} 0 R /Info {info} 0 R >>\nstartxref\n{xref}\n%%EOF").as_bytes(),
    );
    pdf
  }
}

fn flate(raw: &[u8]) -> Vec<u8> {
  archivekit::zlib_compress(raw, archivekit::CompressionLevel::Balanced)
}

fn ascii85(raw: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  for chunk in raw.chunks(4) {
    if chunk == [0, 0, 0, 0] && chunk.len() == 4 {
      out.push(b'z');
      continue;
    }
    let mut value = 0u32;
    for b in chunk {
      value = (value << 8) | u32::from(*b);
    }
    value <<= 8 * (4 - chunk.len());
    let mut digits = [0u8; 5];
    for d in digits.iter_mut().rev() {
      *d = (value % 85) as u8 + b'!';
      value /= 85;
    }
    out.extend_from_slice(&digits[..chunk.len() + 1]);
  }
  out.extend_from_slice(b"~>");
  out
}

fn runlength(raw: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  let mut i = 0;
  while i < raw.len() {
    let run = (i + 128).min(raw.len()) - i;
    out.push((run - 1) as u8);
    out.extend_from_slice(&raw[i..i + run]);
    i += run;
  }
  out.push(128);
  out
}

fn lzw(raw: &[u8]) -> Vec<u8> {
  use std::collections::HashMap;
  let mut dict: HashMap<Vec<u8>, u32> = (0u32..256).map(|b| (vec![b as u8], b)).collect();
  let mut next = 258u32;
  let mut width = 9u32;
  let mut out: Vec<u8> = Vec::new();
  let mut buf = 0u32;
  let mut nbits = 0u32;
  let emit = |code: u32, width: u32, out: &mut Vec<u8>, buf: &mut u32, nbits: &mut u32| {
    *buf = ((*buf << width) | code) & 0xFFFF_FFFF;
    *nbits += width;
    while *nbits >= 8 {
      *nbits -= 8;
      out.push((*buf >> *nbits) as u8);
      *buf &= (1 << *nbits) - 1;
    }
  };
  emit(256, width, &mut out, &mut buf, &mut nbits);
  let mut w = vec![raw[0]];
  for &b in &raw[1..] {
    let mut wc = w.clone();
    wc.push(b);
    if dict.contains_key(&wc) {
      w = wc;
    } else {
      emit(dict[&w], width, &mut out, &mut buf, &mut nbits);
      dict.insert(wc, next);
      next += 1;
      if next == (1 << width) - 1 && width < 12 {
        width += 1;
      }
      w = vec![b];
    }
  }
  emit(dict[&w], width, &mut out, &mut buf, &mut nbits);
  emit(257, width, &mut out, &mut buf, &mut nbits);
  if nbits > 0 {
    out.push((buf << (8 - nbits)) as u8);
  }
  out
}

struct Gen {
  b: Builder,
  entries: Vec<Entry>,
  next: u32,
}

/// Adobe Helvetica AFM advances for codes 32-126.
const HELV: [&str; 95] = [
  "278", "278", "355", "556", "556", "889", "667", "191", "333", "333", "389", "584", "278", "333",
  "278", "278", "556", "556", "556", "556", "556", "556", "556", "556", "556", "556", "278", "278",
  "584", "584", "584", "556", "1015", "667", "667", "722", "722", "667", "611", "778", "722", "278",
  "500", "667", "611", "833", "722", "778", "667", "778", "722", "667", "611", "722", "667", "944",
  "667", "667", "611", "278", "278", "278", "469", "556", "222", "556", "556", "500", "556", "556",
  "278", "556", "556", "222", "222", "500", "222", "833", "556", "556", "556", "556", "333", "500",
  "278", "556", "500", "722", "500", "500", "500", "334", "260", "334", "584",
];

/// Adobe Times-Roman AFM advances for codes 32-126.
const TIMES: [&str; 95] = [
  "250", "333", "408", "500", "500", "833", "778", "333", "333", "333", "500", "564", "250", "333",
  "250", "278", "500", "500", "500", "500", "500", "500", "500", "500", "500", "500", "278", "278",
  "564", "564", "564", "444", "921", "722", "667", "667", "722", "611", "556", "722", "722", "333",
  "389", "722", "611", "889", "722", "722", "611", "722", "667", "556", "611", "722", "667", "889",
  "667", "667", "611", "333", "278", "333", "469", "500", "333", "444", "500", "444", "500", "444",
  "333", "500", "500", "278", "278", "500", "278", "778", "500", "500", "500", "500", "333", "389",
  "278", "500", "500", "722", "500", "500", "444", "480", "200", "480", "541",
];

fn courier_widths() -> String {
  vec!["600"; 95].join(" ")
}

impl Gen {
  fn new() -> Self {
    Self { b: Builder::new(), entries: Vec::new(), next: 1 }
  }

  fn tag(&mut self) -> String {
    let id = format!("E{:03}", self.next);
    self.next += 1;
    id
  }

  fn record(&mut self, id: String, page: usize, rect: [f32; 4], kind: &'static str) {
    self.entries.push(Entry { id, page, rect, kind, verdict: None, ours_rect: None });
  }

  fn record_v(&mut self, id: String, page: usize, rect: [f32; 4], kind: &'static str, verdict: &'static str) {
    self.entries.push(Entry { id, page, rect, kind, verdict: Some(verdict), ours_rect: None });
  }

  fn record_full(
    &mut self,
    id: String,
    page: usize,
    rect: [f32; 4],
    kind: &'static str,
    verdict: Option<&'static str>,
    ours_rect: Option<[f32; 4]>,
  ) {
    self.entries.push(Entry { id, page, rect, kind, verdict, ours_rect });
  }

  fn manifest_json(&self) -> String {
    let mut out = String::from("[\n");
    for (i, e) in self.entries.iter().enumerate() {
      out.push_str(&format!(
        "  {{\"id\":\"{}\",\"page\":{},\"rect\":[{:.1},{:.1},{:.1},{:.1}],\"kind\":\"{}\"",
        e.id, e.page, e.rect[0], e.rect[1], e.rect[2], e.rect[3], e.kind
      ));
      if let Some(v) = e.verdict {
        out.push_str(&format!(",\"verdict\":\"{v}\""));
      }
      if let Some(r) = e.ours_rect {
        out.push_str(&format!(",\"ours_rect\":[{:.1},{:.1},{:.1},{:.1}]", r[0], r[1], r[2], r[3]));
      }
      out.push_str("}");
      if i + 1 < self.entries.len() {
        out.push(',');
      }
      out.push('\n');
    }
    out.push_str("]\n");
    out
  }

  /// One tagged text line. Returns the manifest rect (PDF points).
  fn line(content: &mut Vec<u8>, font: &str, size: f32, x: f32, y: f32, tag: &str, body: &str) -> [f32; 4] {
    let full = format!("[{tag}] {body}");
    content.extend_from_slice(
      format!("BT /{font} {size} Tf {x:.1} {y:.1} Td ({}) Tj ET\n", esc(&full)).as_bytes(),
    );
    line_rect(x, y, size, full.len())
  }

  /// Raw-byte text line (for non-ASCII encodings). `len` is the visible length estimate.
  fn line_raw(content: &mut Vec<u8>, font: &str, size: f32, x: f32, y: f32, tag: &str, prefix: &str, bytes: &[u8]) -> [f32; 4] {
    content.extend_from_slice(format!("BT /{font} {size} Tf {x:.1} {y:.1} Td ([{tag}] {prefix} ").as_bytes());
    content.extend_from_slice(bytes);
    content.extend_from_slice(b") Tj ET\n");
    line_rect(x, y, size, tag.len() + prefix.len() + bytes.len() + 4)
  }

  fn build(&mut self) -> Vec<u8> {
    self.fonts();
    self.page_base_fonts();
    self.page_text_state();
    self.page_encodings();
    self.page_cid();
    self.page_paths_a();
    self.page_paths_b_clip_transform();
    self.page_colors();
    self.page_gstate_shading();
    self.page_images();
    self.page_forms();
    self.page_annots();
    self.page_outlines_info_filters();
    self.page_cropbox();
    self.page_rotate();
    // Catalog, page tree, outlines, info.
    let kids: String = (0..14).map(|i| format!("{} 0 R", 200 + i)).collect::<Vec<_>>().join(" ");
    self.b.raw(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 90 0 R >>");
    self.b.raw(2, &format!("<< /Type /Pages /Kids [{kids}] /Count 14 >>"));
    self.b.raw(90, "<< /First 91 0 R /Last 92 0 R /Count 2 >>");
    self.b.raw(91, "<< /Title (Coverage start) /Parent 90 0 R /Next 92 0 R /Dest [200 0 R /Fit] >>");
    self.b.raw(92, "<< /Title (Coverage forms) /Parent 90 0 R /Prev 91 0 R /Dest [209 0 R /Fit] >>");
    self.b.raw(
      93,
      "<< /Title (PDFKit Coverage) /Creator (coverage generator) /Producer (PDFKit) /Subject (visual compare baseline) >>",
    );
    let builder = std::mem::replace(&mut self.b, Builder::new());
    builder.finish(1, 93)
  }

  fn fonts(&mut self) {
    let b = &mut self.b;
    // Declared Widths are authoritative for advances on both sides,
    // so positions match even though shapes come from different
    // substitute fonts (DejaVu vs SF Pro). Tables are the standard
    // Adobe AFM metrics for 32-126.
    b.raw(3, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 32 /LastChar 126 /Widths [{}] >>", HELV.join(" ")));
    b.raw(4, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /FirstChar 32 /LastChar 126 /Widths [{}] >>", HELV.join(" ")));
    b.raw(5, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Oblique /FirstChar 32 /LastChar 126 /Widths [{}] >>", HELV.join(" ")));
    b.raw(6, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-BoldOblique /FirstChar 32 /LastChar 126 /Widths [{}] >>", HELV.join(" ")));
    b.raw(7, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /FirstChar 32 /LastChar 126 /Widths [{}] >>", TIMES.join(" ")));
    b.raw(8, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Times-Bold /FirstChar 32 /LastChar 126 /Widths [{}] >>", TIMES.join(" ")));
    b.raw(9, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Times-Italic /FirstChar 32 /LastChar 126 /Widths [{}] >>", TIMES.join(" ")));
    b.raw(10, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Times-BoldItalic /FirstChar 32 /LastChar 126 /Widths [{}] >>", TIMES.join(" ")));
    b.raw(11, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Courier /FirstChar 32 /LastChar 126 /Widths [{}] >>", courier_widths()));
    b.raw(12, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Courier-Bold /FirstChar 32 /LastChar 126 /Widths [{}] >>", courier_widths()));
    b.raw(13, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Courier-Oblique /FirstChar 32 /LastChar 126 /Widths [{}] >>", courier_widths()));
    b.raw(14, &format!("<< /Type /Font /Subtype /Type1 /BaseFont /Courier-BoldOblique /FirstChar 32 /LastChar 126 /Widths [{}] >>", courier_widths()));
    b.raw(15, "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>");
    b.raw(16, "<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats >>");
    b.raw(17, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /StandardEncoding >>");
    b.raw(18, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /MacRomanEncoding >>");
    b.raw(
      19,
      "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /BaseEncoding /WinAnsiEncoding /Differences [65 /ae 66 /AE 67 /Euro] >> >>",
    );
    b.stream(
      36,
      "<<",
      b"1 begincodespacerange <00> <FF> endcodespacerange 2 beginbfchar <41> <0042> <43> <D83DDE00> endbfchar 2 beginbfrange <50> <52> <0061> <58> <59> [<006B> <006C>] endbfrange".to_vec(),
    );
    b.raw(20, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 36 0 R >>");
    b.raw(
      33,
      "<< /Type /Font /Subtype /Type0 /BaseFont /Helvetica /Encoding /Identity-H /DescendantFonts [34 0 R] /ToUnicode 35 0 R >>",
    );
    b.raw(
      34,
      "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Helvetica /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 1000 >>",
    );
    b.stream(
      35,
      "<<",
      b"1 begincodespacerange <0000> <FFFF> endcodespacerange 1 beginbfchar <0041> <00480069> endbfchar".to_vec(),
    );
  }

  /// Page 0: one line per base-14 font.
  fn page_base_fonts(&mut self) {
    let mut c = Vec::new();
    let mut y = 748.0;
    let faces = [
      ("F1", "Helvetica ABC abc 123"),
      ("F2", "Helvetica-Bold ABC abc 123"),
      ("F3", "Helvetica-Oblique ABC abc 123"),
      ("F4", "Helvetica-BoldOblique ABC abc 123"),
      ("F5", "Times-Roman ABC abc 123"),
      ("F6", "Times-Bold ABC abc 123"),
      ("F7", "Times-Italic ABC abc 123"),
      ("F8", "Times-BoldItalic ABC abc 123"),
      ("F9", "Courier ABC abc 123"),
      ("F10", "Courier-Bold ABC abc 123"),
      ("F11", "Courier-Oblique ABC abc 123"),
      ("F12", "Courier-BoldOblique ABC abc 123"),
      ("F13", "Symbol ABC abc 123"),
      ("F14", "ZapfDingbats ABC abc 123"),
    ];
    for (font, body) in faces {
      let id = self.tag();
      let rect = Self::line(&mut c, font, 13.0, 56.0, y, &id, &format!("Base font: {body}"));
      self.record(id, 0, rect, "text");
      y -= 22.0;
    }
    self.b.stream(100, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      200,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 100 0 R /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R /F4 6 0 R /F5 7 0 R /F6 8 0 R /F7 9 0 R /F8 10 0 R /F9 11 0 R /F10 12 0 R /F11 13 0 R /F12 14 0 R /F13 15 0 R /F14 16 0 R >> >> >>",
    );
  }

  /// Page 1: text state operators.
  fn page_text_state(&mut self) {
    let mut c = Vec::new();
    let mut y = 748.0;
    // Sizes sweep (one element, several sizes on one line area).
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 8 Tf 56.0 {y:.1} Td ([{id}] sizes 8pt) Tj ET\n").as_bytes(),
    );
    c.extend_from_slice(b"BT /F1 12 Tf 200 748 Td (12pt) Tj ET\n");
    c.extend_from_slice(b"BT /F1 18 Tf 260 748 Td (18pt) Tj ET\n");
    c.extend_from_slice(b"BT /F1 24 Tf 340 748 Td (24pt) Tj ET\n");
    self.record(id, 1, [54.0, 724.0, 480.0, 756.0], "text");
    y -= 30.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 56.0 {y:.1} Td 2 Tc ([{id}] char spacing Tc 2) Tj 0 Tc ET\n").as_bytes(),
    );
    self.record(id, 1, line_rect(56.0, y, 12.0, 30), "text");
    y -= 22.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 56.0 {y:.1} Td 4 Tw ([{id}] word spacing Tw 4PDF) Tj 0 Tw ET\n").as_bytes(),
    );
    self.record(id, 1, line_rect(56.0, y, 12.0, 38), "text");
    y -= 22.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 56.0 {y:.1} Td ([{id}] base) Tj 5 Ts (sup 5) Tj -3 Ts (sub -3) Tj 0 Ts ET\n").as_bytes(),
    );
    self.record(id, 1, line_rect(56.0, y, 12.0, 44), "text");
    y -= 22.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 56.0 {y:.1} Td [([{id}] kern) -60 (TJ kerned)] TJ ET\n").as_bytes(),
    );
    self.record(id, 1, line_rect(56.0, y, 12.0, 26), "text");
    y -= 22.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 1 0 0 1 56.0 {y:.1} Tm ([{id}] placed by Tm) Tj ET\n").as_bytes(),
    );
    self.record(id, 1, line_rect(56.0, y, 12.0, 26), "text");
    y -= 22.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 56.0 {y:.1} Td 100 0 Td ([{id}] moved by Td) Tj ET\n").as_bytes(),
    );
    self.record(id, 1, line_rect(56.0, y, 12.0, 28), "text");
    y -= 22.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 56.0 {y:.1} Td 14 TL ([{id}] first) ' (second via quote) ' ET\n").as_bytes(),
    );
    // Both quoted lines paint below the Td origin: the first `'` steps
    // to y-14, the second to y-28. Cover both runs.
    self.record(id, 1, [54.0, y - 38.0, 360.0, y + 6.0], "text");
    y -= 34.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("q BT /F1 12 Tf 56.0 {y:.1} Td 3 2 ([{id}] quoted dq) \" ET Q\n").as_bytes(),
    );
    self.record(id, 1, line_rect(56.0, y, 12.0, 26), "text");
    y -= 22.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 56.0 {y:.1} Td 0 Tr ([{id}] render mode 0 fill) Tj ET\n").as_bytes(),
    );
    self.record(id, 1, line_rect(56.0, y, 12.0, 34), "text");
    y -= 22.0;
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 56.0 {y:.1} Td 3 Tr ([{id}] invisible) Tj 0 Tr (visible tail) Tj ET\n").as_bytes(),
    );
    self.record(id, 1, line_rect(56.0, y, 12.0, 42), "text");
    y -= 22.0;
    // Rotated text via cm (30 degrees around the label origin).
    // Placed on the clear right side so the diagonal crosses no
    // other element rect. Glyph rotation itself is a documented
    // viewer gap (runs render upright at the translated origin).
    let id = self.tag();
    c.extend_from_slice(
      format!("q 0.866 0.5 -0.5 0.866 400.0 {y:.1} cm BT /F1 14 Tf 0 0 Td ([{id}] rotated 30 deg) Tj ET Q\n").as_bytes(),
    );
    // Estimated bbox: 24 chars * ~7px over 30 degrees from (400, y).
    self.record(id, 1, [394.0, y - 8.0, 580.0, y + 110.0], "text");
    self.b.stream(101, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      201,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 101 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
    );
  }

  /// Page 2: encodings and ToUnicode forms.
  fn page_encodings(&mut self) {
    let mut c = Vec::new();
    let mut y = 748.0;
    let id = self.tag();
    let rect = Self::line(&mut c, "F1", 12.0, 56.0, y, &id, "WinAnsi text: Hello World 123");
    self.record(id, 2, rect, "text");
    y -= 24.0;
    let id = self.tag();
    let rect = Self::line_raw(&mut c, "F1", 14.0, 56.0, y, &id, "WinAnsi specials:", b"\x80\x93\x94\x95\x96\x97 \xE4\xF6\xFC \xC4\xD6\xDC \xDF");
    self.record(id, 2, rect, "text");
    y -= 26.0;
    let id = self.tag();
    let rect = Self::line_raw(&mut c, "F16", 14.0, 56.0, y, &id, "MacRoman 0x80-0x87:", b"\x80\x81\x82\x83\x84\x85\x86\x87");
    self.record(id, 2, rect, "text");
    y -= 26.0;
    let id = self.tag();
    let rect = Self::line(&mut c, "F15", 12.0, 56.0, y, &id, "StandardEncoding sample 123");
    self.record(id, 2, rect, "text");
    y -= 24.0;
    let id = self.tag();
    let rect = Self::line(&mut c, "F17", 14.0, 56.0, y, &id, "Differences A->ae B->AE C->Euro: ABC");
    self.record(id, 2, rect, "text");
    y -= 26.0;
    let id = self.tag();
    let rect = Self::line(&mut c, "F18", 14.0, 56.0, y, &id, "ToUnicode bfchar maps A to B: A");
    self.record(id, 2, rect, "text");
    y -= 26.0;
    let id = self.tag();
    let rect = Self::line(&mut c, "F18", 14.0, 56.0, y, &id, "ToUnicode bfrange PQR to abc: PQR");
    self.record(id, 2, rect, "text");
    y -= 26.0;
    let id = self.tag();
    let rect = Self::line(&mut c, "F18", 14.0, 56.0, y, &id, "ToUnicode array XY to kl: XY and emoji: C");
    self.record(id, 2, rect, "text");
    self.b.stream(102, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      202,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 102 0 R /Resources << /Font << /F1 3 0 R /F15 17 0 R /F16 18 0 R /F17 19 0 R /F18 20 0 R >> >> >>",
    );
  }

  /// Page 3: CID-keyed Type0 font.
  fn page_cid(&mut self) {
    let mut c = Vec::new();
    let id = self.tag();
    c.extend_from_slice(
      format!("BT /F1 12 Tf 56.0 748.0 Td ([{id}] CID Type0 page, Identity-H:) Tj ET\n").as_bytes(),
    );
    self.record(id, 3, line_rect(56.0, 748.0, 12.0, 40), "text");
    let id = self.tag();
    c.extend_from_slice(b"BT /F19 24 Tf 56 700 Td <0041> Tj ET\n");
    c.extend_from_slice(
      format!("BT /F1 10 Tf 56 672 Td ([{id}] above: CID 0041 maps to Hi) Tj ET\n").as_bytes(),
    );
    self.record(id, 3, [54.0, 668.0, 300.0, 730.0], "text");
    let id = self.tag();
    c.extend_from_slice(b"BT /F19 24 Tf 56 620 Td <00410041> Tj ET\n");
    c.extend_from_slice(
      format!("BT /F1 10 Tf 56 592 Td ([{id}] above: doubled CID string) Tj ET\n").as_bytes(),
    );
    self.record(id, 3, [54.0, 588.0, 300.0, 650.0], "text");
    self.b.stream(103, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      203,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 103 0 R /Resources << /Font << /F1 3 0 R /F19 33 0 R >> >> >>",
    );
  }

  /// Page 4: path construction and stroke state.
  fn page_paths_a(&mut self) {
    let mut c = Vec::new();
    let id = self.tag();
    c.extend_from_slice(b"0.5 w 56 716 m 300 716 l S\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 310 712 Td ([{id}] m l stroke) Tj ET\n").as_bytes(),
    );
    self.record_v(id, 4, [54.0, 706.0, 460.0, 726.0], "path", "text");
    let id = self.tag();
    c.extend_from_slice(b"0 0 1 RG 1 w 56 640 m 120 640 120 700 200 700 c S\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 210 668 Td ([{id}] c curve) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [54.0, 634.0, 330.0, 706.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"0 0.5 0 RG 1 w 330 640 m 380 640 400 680 330 680 v S\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 410 668 Td ([{id}] v curve) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [328.0, 634.0, 530.0, 686.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"0.6 0 0.6 RG 1 w 56 560 m 120 600 140 560 150 600 y S\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 160 578 Td ([{id}] y curve) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [54.0, 554.0, 280.0, 606.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"0.8 0.4 0 RG 330 560 120 60 re f\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 330 544 Td ([{id}] h re fill) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [328.0, 538.0, 452.0, 626.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"0 J 0 j [] 0 d 1 w 56 500 m 300 500 l S\n");
    c.extend_from_slice(b"[6 3] 0 d 56 480 m 300 480 l S\n");
    c.extend_from_slice(b"[2 2] 4 d 56 460 m 300 460 l S\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 310 476 Td ([{id}] dash solid dashed dotted) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [54.0, 454.0, 500.0, 506.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"[] 0 d 3 w 0 J 56 400 m 200 400 l S\n");
    c.extend_from_slice(b"1 J 56 380 m 200 380 l S\n");
    c.extend_from_slice(b"2 J 56 360 m 200 360 l S\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 210 376 Td ([{id}] caps butt round square) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [54.0, 350.0, 400.0, 410.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"3 w 0 j 330 340 m 380 400 l 430 340 l S\n");
    c.extend_from_slice(b"1 j 330 280 m 380 340 l 430 280 l S\n");
    c.extend_from_slice(b"2 j 330 220 m 380 280 l 430 220 l S\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 440 306 Td ([{id}] joins miter round bevel) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [322.0, 210.0, 556.0, 410.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"0 j 2 w 10 M 56 300 m 120 360 l 170 300 l 220 360 l S\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 56 284 Td ([{id}] miter limit 10) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [54.0, 278.0, 300.0, 366.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"0.5 w 56 180 m 220 180 l S\n");
    c.extend_from_slice(b"1 w 56 160 m 220 160 l S\n");
    c.extend_from_slice(b"4 w 56 130 m 220 130 l S\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 230 152 Td ([{id}] widths 0.5 1 4) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [54.0, 120.0, 380.0, 190.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"0.8 0.1 0.1 rg 330 60 m 370 150 l 410 60 l 350 120 l 390 30 l 340 90 l 290 30 l 330 120 l h f\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 420 90 Td ([{id}] nonzero star) Tj ET\n").as_bytes(),
    );
    self.record(id, 4, [288.0, 24.0, 556.0, 156.0], "path");
    self.b.stream(104, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(204, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 104 0 R /Resources << /Font << /F1 3 0 R >> >> >>");
  }

  /// Page 5: fill rules, stroking+fill, clip paths and transforms.
  fn page_paths_b_clip_transform(&mut self) {
    let mut c = Vec::new();
    let id = self.tag();
    c.extend_from_slice(b"0.1 0.7 0.2 rg 56 660 m 96 740 l 136 660 l 76 720 l 116 640 l 66 700 l 26 640 l 66 720 l h f*\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 146 690 Td ([{id}] evenodd star) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [24.0, 634.0, 280.0, 746.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"0 g 330 660 m 450 660 l 450 580 l 330 580 l h 360 640 m 420 640 l 420 600 l 360 600 l h f\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 330 560 Td ([{id}] hole nonzero) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [328.0, 554.0, 480.0, 666.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"330 500 m 450 500 l 450 420 l 330 420 l h 360 480 m 420 480 l 420 440 l 360 440 l h f*\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 330 404 Td ([{id}] hole evenodd) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [328.0, 398.0, 480.0, 506.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"0.1 0.3 0.8 rg 0 0 1 RG 2 w 56 560 100 70 re B\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 56 544 Td ([{id}] fill+stroke B) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [54.0, 538.0, 220.0, 636.0], "path");
    let id = self.tag();
    c.extend_from_slice(b"q 180 540 140 90 re W n 0.9 0.2 0.2 rg 150 500 200 170 re f 0 g BT /F1 14 Tf 190 590 Td (in clip) Tj ET Q\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 180 524 Td ([{id}] clip W nonzero) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [148.0, 498.0, 352.0, 636.0], "clip");
    let id = self.tag();
    c.extend_from_slice(b"q 380 540 140 90 re W* n 0.2 0.2 0.9 rg 350 500 200 170 re f Q\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 380 524 Td ([{id}] clip W star evenodd) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [348.0, 498.0, 552.0, 636.0], "clip");
    let id = self.tag();
    c.extend_from_slice(b"q 56 400 160 60 re W n 0 0.5 0 rg BT /F1 16 Tf 66 430 Td (clipped text line) Tj ET Q\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 56 384 Td ([{id}] text inside clip) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [54.0, 378.0, 260.0, 466.0], "clip");
    let id = self.tag();
    c.extend_from_slice(b"q 0.5 0 0 0.5 330 400 cm 0.2 0.5 0.8 rg 0 0 160 100 re f Q\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 330 384 Td ([{id}] nested scale 0.5) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [328.0, 378.0, 500.0, 456.0], "transform");
    let id = self.tag();
    c.extend_from_slice(b"q 0.707 0.707 -0.707 0.707 90 250 cm 0.8 0.5 0.1 rg 0 0 120 50 re f Q\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 56 230 Td ([{id}] nested rotate 45) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [40.0, 224.0, 260.0, 360.0], "transform");
    let id = self.tag();
    c.extend_from_slice(b"q 1 0 0 1 330 250 cm 0.5 0.1 0.7 rg 0 0 120 50 re f Q\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 330 230 Td ([{id}] nested translate) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [328.0, 224.0, 500.0, 306.0], "transform");
    let id = self.tag();
    c.extend_from_slice(b"q 0.6 0.3 -0.3 0.6 80 120 cm 0.1 0.6 0.6 rg 0 0 140 60 re f Q\n");
    c.extend_from_slice(
      format!("BT /F1 9 Tf 80 104 Td ([{id}] combined rotate+scale) Tj ET\n").as_bytes(),
    );
    self.record(id, 5, [60.0, 98.0, 300.0, 220.0], "transform");
    self.b.stream(105, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      205,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 105 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
    );
  }

  /// Page 6: color spaces.
  fn page_colors(&mut self) {
    let mut c = Vec::new();
    // Row 1: gray, rgb, cmyk.
    let id = self.tag();
    c.extend_from_slice(b"0.7 g 56 660 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 644 Td ([{id}] gray) Tj ET\n").as_bytes());
    self.record(id, 6, [54.0, 638.0, 150.0, 716.0], "color");
    let id = self.tag();
    c.extend_from_slice(b"1 0 0 rg 160 660 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 160 644 Td ([{id}] rgb red) Tj ET\n").as_bytes());
    self.record(id, 6, [158.0, 638.0, 254.0, 716.0], "color");
    let id = self.tag();
    c.extend_from_slice(b"1 0 0 0 k 264 660 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 264 644 Td ([{id}] cmyk cyan) Tj ET\n").as_bytes());
    self.record(id, 6, [262.0, 638.0, 358.0, 716.0], "color");
    let id = self.tag();
    c.extend_from_slice(b"/Spot cs 0.5 scn 368 660 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 368 644 Td ([{id}] separation) Tj ET\n").as_bytes());
    self.record(id, 6, [366.0, 638.0, 462.0, 716.0], "color");
    let id = self.tag();
    c.extend_from_slice(b"/Spot cs 0.9 scn 472 660 84 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 472 644 Td ([{id}] sep 0.9) Tj ET\n").as_bytes());
    self.record(id, 6, [470.0, 638.0, 560.0, 716.0], "color");
    // Row 2: indexed x2, calrgb, calgray.
    let id = self.tag();
    c.extend_from_slice(b"/Idx cs 0 scn 56 560 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 544 Td ([{id}] indexed 0) Tj ET\n").as_bytes());
    self.record(id, 6, [54.0, 538.0, 150.0, 616.0], "color");
    let id = self.tag();
    c.extend_from_slice(b"/Idx cs 1 scn 160 560 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 160 544 Td ([{id}] indexed 1) Tj ET\n").as_bytes());
    self.record(id, 6, [158.0, 538.0, 254.0, 616.0], "color");
    let id = self.tag();
    c.extend_from_slice(b"/Cal cs 0.8 0.2 0.2 scn 264 560 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 264 544 Td ([{id}] calrgb) Tj ET\n").as_bytes());
    self.record(id, 6, [262.0, 538.0, 358.0, 616.0], "color");
    let id = self.tag();
    c.extend_from_slice(b"/CalG cs 0.35 scn 368 560 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 368 544 Td ([{id}] calgray) Tj ET\n").as_bytes());
    self.record(id, 6, [366.0, 538.0, 462.0, 616.0], "color");
    // Row 3: lab, iccbased.
    let id = self.tag();
    c.extend_from_slice(b"/LabC cs 70 20 -30 scn 56 460 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 444 Td ([{id}] lab) Tj ET\n").as_bytes());
    self.record(id, 6, [54.0, 438.0, 150.0, 516.0], "color");
    let id = self.tag();
    c.extend_from_slice(b"/Icc cs 0.2 0.6 0.9 scn 160 460 90 50 re f\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 160 444 Td ([{id}] icc alternate) Tj ET\n").as_bytes());
    self.record(id, 6, [158.0, 438.0, 300.0, 516.0], "color");
    self.b.stream(106, "<< /Filter /FlateDecode", flate(&c));
    self.b.stream(162, "<< /N 3 /Alternate /DeviceRGB", b"dummy-profile-bytes-ignored-via-alternate".to_vec());
    self.b.raw(
      206,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 106 0 R /Resources << /Font << /F1 3 0 R >> /ColorSpace << /Spot [/Separation /Logo /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 1] /C1 [0 1 1 0] /N 1 >>] /Idx [/Indexed /DeviceRGB 1 <FF000000FF00>] /Cal [/CalRGB << /Gamma [2.2 2.2 2.2] >>] /CalG [/CalGray << /Gamma 2.2 >>] /LabC [/Lab << /WhitePoint [0.95 1 1.09] /Range [-100 100 -100 100] >>] /Icc [/ICCBased 162 0 R] >> >> >>",
    );
  }

  /// Page 7: transparency and shadings.
  fn page_gstate_shading(&mut self) {
    let mut c = Vec::new();
    let id = self.tag();
    c.extend_from_slice(b"q /GS1 gs 1 0 0 rg 56 640 140 90 re f 0 0 1 rg 126 600 140 90 re f Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 584 Td ([{id}] ca 0.5 overlap) Tj ET\n").as_bytes());
    self.record(id, 7, [54.0, 578.0, 280.0, 736.0], "gstate");
    let id = self.tag();
    c.extend_from_slice(b"q /GS2 gs 0 0.6 0 RG 4 w 56 500 140 60 re S 0.8 0 0 RG 126 470 140 60 re S Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 454 Td ([{id}] CA stroke overlap) Tj ET\n").as_bytes());
    self.record(id, 7, [54.0, 448.0, 280.0, 566.0], "gstate");
    let id = self.tag();
    c.extend_from_slice(b"q /GS3 gs 0.9 0.6 0 rg 340 640 140 90 re f 0.2 0.4 0.9 rg 410 600 140 90 re f Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 340 584 Td ([{id}] ca+CA combined) Tj ET\n").as_bytes());
    self.record(id, 7, [338.0, 578.0, 556.0, 736.0], "gstate");
    let id = self.tag();
    c.extend_from_slice(b"/Ax1 sh\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 404 Td ([{id}] axial shading red to blue) Tj ET\n").as_bytes());
    self.record(id, 7, [54.0, 354.0, 310.0, 440.0], "shading");
    let id = self.tag();
    c.extend_from_slice(b"/Rd1 sh\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 340 250 Td ([{id}] radial shading white to black) Tj ET\n").as_bytes());
    self.record(id, 7, [338.0, 200.0, 556.0, 420.0], "shading");
    self.b.stream(107, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      160,
      "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [56 390 300 390] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true true] >>",
    );
    self.b.raw(
      161,
      "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [450 340 5 450 340 70] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [0 0 0] /N 1 >> /Extend [true true] >>",
    );
    self.b.raw(
      207,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 107 0 R /Resources << /Font << /F1 3 0 R >> /ExtGState << /GS1 << /ca 0.5 /CA 1 >> /GS2 << /ca 1 /CA 0.4 >> /GS3 << /ca 0.6 /CA 0.7 >> >> /Shading << /Ax1 160 0 R /Rd1 161 0 R >> >> >>",
    );
  }

  /// Page 8: images.
  fn page_images(&mut self) {
    let rgb_bytes: Vec<u8> = (0..16)
      .flat_map(|y| (0..16).flat_map(move |x| vec![(x * 16) as u8, (y * 16) as u8, 128u8]))
      .collect();
    let checker: Vec<u8> = vec![0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55];
    let idx_img: Vec<u8> = vec![0xAA];
    let smask: Vec<u8> = (0..8).flat_map(|y| vec![(y * 36) as u8; 8]).collect();
    let photo: Vec<u8> = (0..8).flat_map(|y| (0..8).flat_map(move |x| vec![(x * 32) as u8, (y * 32) as u8, 200u8])).collect();
    let b = &mut self.b;
    b.stream(150, "<< /Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode", flate(&rgb_bytes));
    b.stream(151, "<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /FlateDecode", flate(&checker));
    b.stream(
      152,
      "<< /Type /XObject /Subtype /Image /Width 8 /Height 1 /ColorSpace [/Indexed /DeviceRGB 1 <FF000000FF00>] /BitsPerComponent 1 /Filter /FlateDecode",
      flate(&idx_img),
    );
    b.stream(153, "<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode", flate(&smask));
    b.stream(154, "<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 153 0 R /Filter /FlateDecode", flate(&photo));
    b.stream(
      155,
      "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 1 /ImageMask true /Filter /FlateDecode",
      flate(&[0x80, 0x80]),
    );
    let mut c = Vec::new();
    let id = self.tag();
    c.extend_from_slice(b"q 64 0 0 64 56 640 cm /ImRGB Do Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 624 Td ([{id}] rgb flate) Tj ET\n").as_bytes());
    self.record(id, 8, [54.0, 618.0, 200.0, 710.0], "image");
    let id = self.tag();
    c.extend_from_slice(b"q 48 0 0 48 220 640 cm /ImGray Do Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 220 624 Td ([{id}] gray 1-bit) Tj ET\n").as_bytes());
    self.record(id, 8, [218.0, 618.0, 360.0, 694.0], "image");
    let id = self.tag();
    c.extend_from_slice(b"q 64 0 0 32 340 640 cm /ImIdx Do Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 340 624 Td ([{id}] indexed) Tj ET\n").as_bytes());
    self.record(id, 8, [338.0, 618.0, 480.0, 678.0], "image");
    let id = self.tag();
    c.extend_from_slice(b"q 64 0 0 64 56 520 cm /ImPhoto Do Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 504 Td ([{id}] smask photo) Tj ET\n").as_bytes());
    self.record(id, 8, [54.0, 498.0, 200.0, 590.0], "image");
    let id = self.tag();
    c.extend_from_slice(b"0.8 0.2 0.2 rg q 32 0 0 32 220 520 cm /ImStencil Do Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 220 504 Td ([{id}] stencil mask) Tj ET\n").as_bytes());
    self.record(id, 8, [218.0, 498.0, 360.0, 558.0], "image");
    let id = self.tag();
    c.extend_from_slice(b"q 48 0 0 48 340 520 cm BI /W 2 /H 2 /CS /RGB /BPC 8 ID \xFF\x00\x00\x00\xFF\x00\x00\x00\xFF\xFF\xFF\xFF EI Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 340 504 Td ([{id}] inline image) Tj ET\n").as_bytes());
    self.record(id, 8, [338.0, 498.0, 480.0, 574.0], "image");
    self.b.stream(108, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      208,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 108 0 R /Resources << /Font << /F1 3 0 R >> /XObject << /ImRGB 150 0 R /ImGray 151 0 R /ImIdx 152 0 R /ImPhoto 154 0 R /ImStencil 155 0 R >> >> >>",
    );
  }

  /// Page 9: nested forms.
  fn page_forms(&mut self) {
    let mut fb = Vec::new();
    fb.extend_from_slice(b"0.2 0.6 0.2 RG 2 w 0 0 100 60 re S\n");
    fb.extend_from_slice(b"BT /F1 12 Tf 10 25 Td (Form B) Tj ET\n");
    self.b.stream(
      141,
      "<< /Type /XObject /Subtype /Form /BBox [0 0 100 60] /Resources << /Font << /F1 3 0 R >> >> /Filter /FlateDecode",
      flate(&fb),
    );
    let mut fa = Vec::new();
    fa.extend_from_slice(b"q 1 0 0 1 0 70 cm /FmB Do Q\n");
    fa.extend_from_slice(b"0.8 0.1 0.1 rg 0 0 140 60 re f\n");
    fa.extend_from_slice(b"BT /F1 10 Tf 5 5 Td (Form A wraps B) Tj ET\n");
    self.b.stream(
      140,
      "<< /Type /XObject /Subtype /Form /BBox [0 0 140 140] /Resources << /Font << /F1 3 0 R >> /XObject << /FmB 141 0 R >> >> /Filter /FlateDecode",
      flate(&fa),
    );
    let mut c = Vec::new();
    let id = self.tag();
    c.extend_from_slice(b"q 1 0 0 1 56 450 cm /FmB Do Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 434 Td ([{id}] form B direct) Tj ET\n").as_bytes());
    self.record(id, 9, [54.0, 428.0, 220.0, 516.0], "form");
    let id = self.tag();
    c.extend_from_slice(b"q 1 0 0 1 56 600 cm /FmA Do Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 56 584 Td ([{id}] form A instance 1) Tj ET\n").as_bytes());
    self.record(id, 9, [54.0, 578.0, 260.0, 746.0], "form");
    let id = self.tag();
    c.extend_from_slice(b"q 0.6 0 0 0.6 280 600 cm /FmA Do Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 280 584 Td ([{id}] form A scaled 0.6) Tj ET\n").as_bytes());
    self.record(id, 9, [278.0, 578.0, 470.0, 690.0], "form");
    let id = self.tag();
    c.extend_from_slice(b"q 0.7 0.7 -0.7 0.7 420 420 cm /FmA Do Q\n");
    c.extend_from_slice(format!("BT /F1 9 Tf 420 404 Td ([{id}] form A rotated) Tj ET\n").as_bytes());
    self.record(id, 9, [320.0, 398.0, 556.0, 560.0], "form");
    self.b.stream(109, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      209,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 109 0 R /Resources << /Font << /F1 3 0 R >> /XObject << /FmA 140 0 R /FmB 141 0 R >> >> >>",
    );
  }

  /// Page 10: annotations with visible anchors.
  fn page_annots(&mut self) {
    let mut c = Vec::new();
    let mut y = 700.0;
    let id = self.tag();
    c.extend_from_slice(format!("BT /F1 12 Tf 56.0 {y:.1} Td ([{id}] Link to example.com:) Tj ET\n").as_bytes());
    self.b.raw(
      130,
      &format!("<< /Type /Annot /Subtype /Link /Rect [56 {:.1} 260 {:.1}] /Border [0 0 1] /A << /S /URI /URI (https://example.com) >> >>", y - 4.0, y + 14.0),
    );
    self.record_v(id, 10, [54.0, y - 6.0, 262.0, y + 16.0], "annot", "text");
    y -= 34.0;
    let id = self.tag();
    c.extend_from_slice(format!("BT /F1 12 Tf 56.0 {y:.1} Td ([{id}] Link to page 1 (GoTo):) Tj ET\n").as_bytes());
    self.b.raw(131, &format!("<< /Type /Annot /Subtype /Link /Rect [56 {:.1} 260 {:.1}] /Border [0 0 0] /Dest [200 0 R /Fit] >>", y - 4.0, y + 14.0));
    self.record_v(id, 10, [54.0, y - 6.0, 262.0, y + 16.0], "annot", "text");
    y -= 34.0;
    let id = self.tag();
    c.extend_from_slice(format!("BT /F1 12 Tf 56.0 {y:.1} Td ([{id}] Highlight this line) Tj ET\n").as_bytes());
    self.b.raw(
      132,
      &format!("<< /Type /Annot /Subtype /Highlight /Rect [56 {:.1} 260 {:.1}] /C [1 1 0] /QuadPoints [56 {:.1} 260 {:.1} 260 {:.1} 56 {:.1}] >>", y - 4.0, y + 14.0, y + 14.0, y + 14.0, y - 4.0, y - 4.0),
    );
    self.record_v(id, 10, [54.0, y - 6.0, 262.0, y + 16.0], "annot", "text");
    y -= 34.0;
    let id = self.tag();
    c.extend_from_slice(format!("BT /F1 12 Tf 56.0 {y:.1} Td ([{id}] Underline this line) Tj ET\n").as_bytes());
    self.b.raw(
      133,
      &format!("<< /Type /Annot /Subtype /Underline /Rect [56 {:.1} 260 {:.1}] /C [1 0 0] /QuadPoints [56 {:.1} 260 {:.1} 260 {:.1} 56 {:.1}] >>", y - 4.0, y + 14.0, y + 14.0, y + 14.0, y - 4.0, y - 4.0),
    );
    self.record_v(id, 10, [54.0, y - 6.0, 262.0, y + 16.0], "annot", "text");
    y -= 34.0;
    let id = self.tag();
    c.extend_from_slice(format!("BT /F1 12 Tf 56.0 {y:.1} Td ([{id}] StrikeOut this line) Tj ET\n").as_bytes());
    self.b.raw(
      134,
      &format!("<< /Type /Annot /Subtype /StrikeOut /Rect [56 {:.1} 260 {:.1}] /C [0 0.6 0] /QuadPoints [56 {:.1} 260 {:.1} 260 {:.1} 56 {:.1}] >>", y - 4.0, y + 14.0, y + 14.0, y + 14.0, y - 4.0, y - 4.0),
    );
    self.record_v(id, 10, [54.0, y - 6.0, 262.0, y + 16.0], "annot", "text");
    y -= 60.0;
    let id = self.tag();
    c.extend_from_slice(format!("BT /F1 12 Tf 56.0 {y:.1} Td ([{id}] Square annot below:) Tj ET\n").as_bytes());
    self.b.raw(135, "<< /Type /Annot /Subtype /Square /Rect [56 400 200 460] /C [0 0 1] /Border [0 0 2] >>");
    self.record(id, 10, [54.0, 384.0, 280.0, 462.0], "annot");
    let id = self.tag();
    c.extend_from_slice(format!("BT /F1 12 Tf 280.0 {y:.1} Td ([{id}] Circle annot below:) Tj ET\n").as_bytes());
    self.b.raw(136, "<< /Type /Annot /Subtype /Circle /Rect [280 400 424 460] /C [0 0.6 0] /Border [0 0 2] >>");
    self.record_v(id, 10, [278.0, 384.0, 470.0, 462.0], "annot", "text");
    let id = self.tag();
    c.extend_from_slice(format!("BT /F1 12 Tf 56.0 360.0 Td ([{id}] Ink annot below:) Tj ET\n").as_bytes());
    self.b.raw(137, "<< /Type /Annot /Subtype /Ink /Rect [56 280 300 340] /C [0.5 0 0.5] /InkList [[56 330 120 310 170 330 220 305 290 325]] >>");
    self.record(id, 10, [54.0, 274.0, 310.0, 362.0], "annot");
    let id = self.tag();
    c.extend_from_slice(format!("BT /F1 12 Tf 320.0 360.0 Td ([{id}] Text note marker:) Tj ET\n").as_bytes());
    self.b.raw(138, "<< /Type /Annot /Subtype /Text /Rect [320 320 340 340] /Contents (Sticky note) /C [1 1 0] >>");
    // Rect top clears the 12pt label (baseline y=360) so the whole
    // anchor line is inside the crop.
    self.record_v(id, 10, [318.0, 314.0, 500.0, 374.0], "annot", "text");
    self.b.stream(110, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      210,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 110 0 R /Resources << /Font << /F1 3 0 R >> >> /Annots [130 0 R 131 0 R 132 0 R 133 0 R 134 0 R 135 0 R 136 0 R 137 0 R 138 0 R] >>",
    );
  }

  /// Page 11: outline/info anchors plus one stream per filter.
  fn page_outlines_info_filters(&mut self) {
    let mut anchors = Vec::new();
    let id = self.tag();
    let rect = Self::line(&mut anchors, "F1", 12.0, 56.0, 748.0, &id, "Outline target 1: Coverage start (see bookmarks)");
    self.record_v(id, 11, rect, "outline", "text");
    let id = self.tag();
    let rect = Self::line(&mut anchors, "F1", 12.0, 56.0, 724.0, &id, "Outline target 2: Coverage forms (see bookmarks)");
    self.record_v(id, 11, rect, "outline", "text");
    let id = self.tag();
    let rect = Self::line(&mut anchors, "F1", 12.0, 56.0, 700.0, &id, "Info Title is PDFKit Coverage (see document properties)");
    self.record_v(id, 11, rect, "info", "text");
    let id = self.tag();
    let rect = Self::line(&mut anchors, "F1", 12.0, 56.0, 676.0, &id, "FlateDecode stream part below");
    self.record_v(id, 11, rect, "filter", "text");
    self.b.stream(120, "<< /Filter /FlateDecode", flate(&anchors));
    let id = self.tag();
    let l2 = format!("BT /F1 14 Tf 56 640 Td ([{id}] ASCII85Decode stream part) Tj ET\n");
    self.b.stream(121, "<< /Filter /ASCII85Decode", ascii85(l2.as_bytes()));
    self.record_v(id, 11, line_rect(56.0, 640.0, 14.0, 40), "filter", "text");
    let id = self.tag();
    let l3 = format!("BT /F1 14 Tf 56 610 Td ([{id}] RunLengthDecode stream part) Tj ET\n");
    self.b.stream(122, "<< /Filter /RunLengthDecode", runlength(l3.as_bytes()));
    self.record_v(id, 11, line_rect(56.0, 610.0, 14.0, 42), "filter", "text");
    let id = self.tag();
    let l4 = format!("BT /F1 14 Tf 56 580 Td ([{id}] LZWDecode stream part with enough text to grow the table beyond nine bit codes a few times over and over) Tj ET\n");
    self.b.stream(123, "<< /Filter /LZWDecode", lzw(l4.as_bytes()));
    self.record_v(id, 11, line_rect(56.0, 580.0, 14.0, 110), "filter", "text");
    self.b.raw(
      211,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [120 0 R 121 0 R 122 0 R 123 0 R] /Resources << /Font << /F1 3 0 R >> >> >>",
    );
  }

  /// Page 12: CropBox (content below the crop is intentionally clipped
  /// by real viewers; our renderer clips to the CropBox while poppler
  /// rasters the full MediaBox, so E102 carries an ours-side rect and
  /// E103 asserts absence).
  fn page_cropbox(&mut self) {
    let mut c = Vec::new();
    let id = self.tag();
    let rect = Self::line(&mut c, "F1", 12.0, 80.0, 700.0, &id, "CropBox page: this line is inside the crop");
    // Our page 12 is CropBox-relative (origin 72, 400): shift the crop.
    let ours = [rect[0] - 72.0, rect[1] - 400.0, rect[2] - 72.0, rect[3] - 400.0];
    self.record_full(id, 12, rect, "crop", Some("text"), Some(ours));
    let id = self.tag();
    let rect = Self::line(&mut c, "F1", 12.0, 80.0, 200.0, &id, "BELOW CROP: clipped by viewers, visible if CropBox ignored");
    // Correctly clipped away on our side: assert absence.
    self.record_v(id, 12, rect, "crop", "absent");
    self.b.stream(112, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      212,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /CropBox [72 400 540 792] /Contents 112 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
    );
  }

  /// Page 13: /Rotate 90 (applied by the view mapping; manifest
  /// rects live in the rotated display frame, verdict by ink edges).
  fn page_rotate(&mut self) {
    // Display frame for /Rotate 90 on a 612-wide page: user (x, y)
    // shows at display (y, 612 - x), so bboxes map accordingly.
    fn rot90(b: [f32; 4]) -> [f32; 4] {
      [b[1], 612.0 - b[2], b[3], 612.0 - b[0]]
    }
    let mut c = Vec::new();
    let id = self.tag();
    let full = format!("[{id}] rotate 90 line");
    c.extend_from_slice(format!("BT /F1 12 Tf 56.0 700.0 Td ({}) Tj ET\n", esc(&full)).as_bytes());
    let rect = rot90(line_rect(56.0, 700.0, 12.0, full.len()));
    self.record_v(id, 13, rect, "rotate", "text");
    let id = self.tag();
    c.extend_from_slice(b"0.7 0.1 0.1 rg 56 560 200 100 re f\n");
    c.extend_from_slice(format!("BT /F1 10 Tf 66 600 Td ([{id}] rect under rotate 90) Tj ET\n").as_bytes());
    let rect = rot90([54.0, 554.0, 330.0, 666.0]);
    self.record_v(id, 13, rect, "rotate", "text");
    self.b.stream(113, "<< /Filter /FlateDecode", flate(&c));
    self.b.raw(
      213,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 /Contents 113 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
    );
  }
}

/// Escape a PDF literal string body.
fn esc(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for ch in s.chars() {
    match ch {
      '\\' => out.push_str("\\\\"),
      '(' => out.push_str("\\("),
      ')' => out.push_str("\\)"),
      _ => out.push(ch),
    }
  }
  out
}

/// Estimated manifest rect for a text line at (x, baseline y).
fn line_rect(x: f32, y: f32, size: f32, chars: usize) -> [f32; 4] {
  let w = chars as f32 * size * 0.55;
  [x - 2.0, y - size * 0.35, x + w + 4.0, y + size * 1.25]
}
