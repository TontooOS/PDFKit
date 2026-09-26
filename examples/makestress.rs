use std::io::Write as IoWrite;

/// Stress PDF generator for side-by-side comparison (e.g. Firefox).
///
/// Usage: `cargo run --example makestress -- [out.pdf]`
/// Default output is `stress.pdf` next to the crate.
fn main() {
  let out = std::env::args().nth(1).unwrap_or_else(|| String::from("stress.pdf"));
  let pdf = build();
  std::fs::write(&out, &pdf).unwrap();
  println!("wrote {} ({} bytes, 10 pages)", out, pdf.len());
  // Self-check: load it back with PDFKit and summarize every page.
  let doc = pdfkit::PdfDocument::load_bytes(pdf).expect("stress file must parse");
  assert_eq!(doc.page_count(), 10);
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
  assert!(texts > 40 && paths > 15 && grads == 2 && images == 6 && annots == 8);
  println!("self-check ok");
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
  let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  enc.write_all(raw).unwrap();
  enc.finish().unwrap()
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
  emit(257, width, &mut out, &mut buf, &mut nbits);
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
  emit(256, width, &mut out, &mut buf, &mut nbits);
  if nbits > 0 {
    out.push((buf << (8 - nbits)) as u8);
  }
  out
}

fn build() -> Vec<u8> {
  let mut b = Builder::new();
  // Catalog, pages, info, outlines.
  b.raw(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 90 0 R >>");
  b.raw(
    2,
    "<< /Type /Pages /Kids [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 15 0 R 16 0 R 17 0 R 18 0 R 19 0 R] /Count 10 >>",
  );
  b.raw(90, "<< /First 91 0 R /Last 92 0 R /Count 2 >>");
  b.raw(91, "<< /Title (Text Basics) /Parent 90 0 R /Next 92 0 R /Dest [10 0 R /Fit] >>");
  b.raw(92, "<< /Title (Pictures) /Parent 90 0 R /Prev 91 0 R /Dest [16 0 R /Fit] >>");
  b.raw(93, "<< /Title (PDFKit Stress) /Creator (makestress) /Producer (PDFKit) >>");
  // Fonts.
  b.raw(3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
  b.raw(4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>");
  b.raw(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>");
  b.raw(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>");
  b.raw(
    7,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /BaseEncoding /WinAnsiEncoding /Differences [65 /ae 66 /AE 67 /Euro] >> >>",
  );
  b.raw(8, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /MacRomanEncoding >>");
  b.raw(
    9,
    "<< /Type /Font /Subtype /Type0 /BaseFont /Helvetica /Encoding /Identity-H /DescendantFonts [40 0 R] /ToUnicode 41 0 R >>",
  );
  b.raw(
    40,
    "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Helvetica /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 1000 >>",
  );
  b.stream(
    41,
    "<<",
    b"1 begincodespacerange <0000> <FFFF> endcodespacerange 1 beginbfchar <0041> <00480069> endbfchar".to_vec(),
  );

  // Page 1: text basics.
  let mut p1 = Vec::new();
  p1.extend_from_slice(b"BT /F1 24 Tf 72 750 Td (Text Basics) Tj ET\n");
  p1.extend_from_slice(b"BT /F1 12 Tf 72 720 Td (Hello, World!) Tj ET\n");
  p1.extend_from_slice(b"BT /F2 12 Tf 72 700 Td (Bold via BaseFont) Tj ET\n");
  p1.extend_from_slice(b"BT /F3 14 Tf 72 680 Td (Times Roman 14pt) Tj ET\n");
  p1.extend_from_slice(b"BT /F4 10 Tf 72 660 Td (Courier 10pt monospace) Tj ET\n");
  p1.extend_from_slice(b"BT /F1 12 Tf 72 630 Td [(W) -80 (a) 40 (ter TJ kerning)] TJ ET\n");
  p1.extend_from_slice(b"BT /F1 12 Tf 72 610 Td 14 TL (first line) ' (second line via quote) ' ET\n");
  p1.extend_from_slice(b"BT /F1 12 Tf 72 570 Td 2 Tw 1 Tc (wide spaced) Tj 0 Tw 0 Tc ET\n");
  p1.extend_from_slice(b"BT /F1 12 Tf 72 550 Td (x) Tj 4 Ts (2 is superscript) Tj 0 Ts ET\n");
  p1.extend_from_slice(b"BT /F1 12 Tf 72 530 Td 3 Tr (invisible OCR text) Tj 0 Tr (visible again) Tj ET\n");
  p1.extend_from_slice(b"BT /F1 12 Tf 1 0 0 1 72 500 Tm (placed by Tm) Tj ET\n");
  p1.extend_from_slice(b"BT /F1 12 Tf 72 480 Td 4 2 (quoted with spaces) \" ET\n");
  b.stream(20, "<< /Filter /FlateDecode", flate(&p1));
  b.raw(
    10,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 20 0 R /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R /F4 6 0 R >> >> >>",
  );

  // Page 2: encodings.
  let mut p2 = Vec::new();
  p2.extend_from_slice(b"BT /F1 24 Tf 72 750 Td (Encodings) Tj ET\n");
  p2.extend_from_slice(b"BT /F1 12 Tf 72 720 Td (WinAnsi Euro + quotes + umlauts:) Tj ET\n");
  p2.extend_from_slice(b"BT /F1 14 Tf 72 700 Td (\x80\x93\x94\x95\x96\x97 \xE4\xF6\xFC \xC4\xD6\xDC \xDF) Tj ET\n");
  p2.extend_from_slice(b"BT /F1 12 Tf 72 670 Td (Differences A->ae B->AE C->Euro:) Tj ET\n");
  p2.extend_from_slice(b"BT /F7 14 Tf 72 650 Td (ABC) Tj ET\n");
  p2.extend_from_slice(b"BT /F1 12 Tf 72 620 Td (MacRoman 0x80-0x83:) Tj ET\n");
  p2.extend_from_slice(b"BT /F8 14 Tf 72 600 Td (\x80\x81\x82\x83) Tj ET\n");
  p2.extend_from_slice(b"BT /F1 12 Tf 72 570 Td (ToUnicode bfchar + bfrange + array + emoji:) Tj ET\n");
  b.stream(
    42,
    "<<",
    b"1 begincodespacerange <00> <FF> endcodespacerange 2 beginbfchar <41> <0042> <43> <D83DDE00> endbfchar 2 beginbfrange <50> <52> <0061> <58> <59> [<006B> <006C>] endbfrange".to_vec(),
  );
  b.raw(
    43,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 42 0 R >>",
  );
  p2.extend_from_slice(b"BT /F9 14 Tf 72 550 Td (ACPQRXY) Tj ET\n");
  b.stream(21, "<< /Filter /FlateDecode", flate(&p2));
  b.raw(
    11,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 21 0 R /Resources << /Font << /F1 3 0 R /F7 7 0 R /F8 8 0 R /F9 43 0 R >> >> >>",
  );

  // Page 3: CID Type0.
  let mut p3 = Vec::new();
  p3.extend_from_slice(b"BT /F1 24 Tf 72 750 Td (CID Type0) Tj ET\n");
  p3.extend_from_slice(b"BT /F10 20 Tf 72 700 Td <0041> Tj ET\n");
  b.stream(22, "<< /Filter /FlateDecode", flate(&p3));
  b.raw(
    12,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 22 0 R /Resources << /Font << /F1 3 0 R /F10 9 0 R >> >> >>",
  );

  // Page 4: paths.
  let mut p4 = Vec::new();
  p4.extend_from_slice(b"BT /F1 24 Tf 72 750 Td (Paths) Tj ET\n");
  p4.extend_from_slice(b"0.5 w 72 700 m 300 700 l S\n");
  p4.extend_from_slice(b"2 w 1 J 72 680 m 300 680 l S\n");
  p4.extend_from_slice(b"0 J [6 3] 0 d 72 660 m 300 660 l S\n");
  p4.extend_from_slice(b"[] 0 d 1 j 3 w 72 560 m 120 620 l 170 560 l 220 620 l S\n");
  p4.extend_from_slice(b"0 j 1 w 72 480 m 150 480 150 560 230 560 c S\n");
  p4.extend_from_slice(b"72 440 m 100 440 120 470 72 470 v S\n");
  p4.extend_from_slice(b"200 440 m 230 470 250 440 260 470 y S\n");
  p4.extend_from_slice(b"0.8 0.1 0.1 rg 320 560 120 80 re f\n");
  p4.extend_from_slice(b"0.1 0.3 0.8 rg 320 470 120 80 re B\n");
  p4.extend_from_slice(b"0.1 0.7 0.2 rg 320 340 m 380 460 l 440 340 l 360 420 l 400 300 l 340 380 l 280 300 l 320 420 l h f*\n");
  p4.extend_from_slice(b"0 rg 72 340 m 200 340 l 200 260 l 72 260 l h 100 320 m 170 320 l 170 280 l 100 280 l h f\n");
  p4.extend_from_slice(b"72 230 m 200 230 l 200 150 l 72 150 l h 100 210 m 170 210 l 170 170 l 100 170 l h f*\n");
  b.stream(23, "<< /Filter /FlateDecode", flate(&p4));
  b.raw(13, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 23 0 R >>");

  // Page 5: transforms + clip.
  let mut p5 = Vec::new();
  p5.extend_from_slice(b"BT /F1 24 Tf 72 750 Td (Transforms + Clip) Tj ET\n");
  p5.extend_from_slice(b"q 0.707 0.707 -0.707 0.707 200 500 cm BT /F1 16 Tf 0 0 Td (rotated 45) Tj ET Q\n");
  p5.extend_from_slice(b"q 0.5 0 0 0.5 320 500 cm 0 0 100 60 re f Q\n");
  p5.extend_from_slice(b"q 100 400 200 120 re W n 0.9 0.2 0.2 rg 50 350 300 200 re f 0 rg BT /F1 20 Tf 80 430 Td (clipped text here) Tj ET Q\n");
  p5.extend_from_slice(b"q 300 300 150 150 re W* n 0.2 0.2 0.9 rg 250 250 250 250 re f Q\n");
  b.stream(24, "<< /Filter /FlateDecode", flate(&p5));
  b.raw(14, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 24 0 R /Resources << /Font << /F1 3 0 R >> >> >>");

  // Page 6: colors + transparency + shading.
  let mut p6 = Vec::new();
  p6.extend_from_slice(b"BT /F1 24 Tf 72 750 Td (Colors) Tj ET\n");
  p6.extend_from_slice(b"0.7 g 72 700 60 40 re f\n");
  p6.extend_from_slice(b"1 0 0 rg 150 700 60 40 re f\n");
  p6.extend_from_slice(b"1 0 0 0 k 228 700 60 40 re f\n");
  p6.extend_from_slice(b"/Spot cs 0.5 scn 306 700 60 40 re f\n");
  p6.extend_from_slice(b"/Idx cs 0 scn 384 700 60 40 re f /Idx cs 1 scn 450 700 60 40 re f\n");
  p6.extend_from_slice(b"/Cal cs 0.8 0.2 0.2 scn 72 640 60 40 re f\n");
  p6.extend_from_slice(b"/LabC cs 70 20 -30 scn 150 640 60 40 re f\n");
  p6.extend_from_slice(b"/Icc cs 0.2 0.6 0.9 scn 228 640 60 40 re f\n");
  p6.extend_from_slice(b"q /GS1 gs 1 0 0 rg 72 560 120 80 re f 0 0 1 rg 132 520 120 80 re f Q\n");
  p6.extend_from_slice(b"/Ax1 sh\n");
  p6.extend_from_slice(b"/Rd1 sh\n");
  b.stream(25, "<< /Filter /FlateDecode", flate(&p6));
  b.stream(
    60,
    "<< /N 3 /Alternate /DeviceRGB",
    b"dummy-profile-bytes-ignored-via-alternate".to_vec(),
  );
  b.raw(
    61,
    "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [72 430 300 430] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true true] >>",
  );
  b.raw(
    62,
    "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [450 350 5 450 350 70] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [0 0 0] /N 1 >> /Extend [true true] >>",
  );
  b.raw(
    15,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 25 0 R /Resources << /Font << /F1 3 0 R >> /ColorSpace << /Spot [/Separation /Logo /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 1] /C1 [0 1 1 0] /N 1 >>] /Idx [/Indexed /DeviceRGB 1 <FF000000FF00>] /Cal [/CalRGB << /Gamma [2.2 2.2 2.2] >>] /LabC [/Lab << /WhitePoint [0.95 1 1.09] /Range [-100 100 -100 100] >>] /Icc [/ICCBased 60 0 R] >> /ExtGState << /GS1 << /ca 0.5 /CA 0.8 >> >> /Shading << /Ax1 61 0 R /Rd1 62 0 R >> >> >>",
  );

  // Page 7: images.
  // 16x16 RGB gradient, 3 bytes per pixel.
  let rgb_bytes: Vec<u8> = (0..16)
    .flat_map(|y| (0..16).flat_map(move |x| vec![(x * 16) as u8, (y * 16) as u8, 128u8]))
    .collect();
  let checker: Vec<u8> = vec![0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55];
  let idx_img: Vec<u8> = vec![0xAA];
  let smask: Vec<u8> = (0..8).flat_map(|y| vec![(y * 36) as u8; 8]).collect();
  let photo: Vec<u8> = (0..8).flat_map(|y| (0..8).flat_map(move |x| vec![(x * 32) as u8, (y * 32) as u8, 200u8])).collect();
  b.stream(63, "<< /Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode", flate(&rgb_bytes));
  b.stream(64, "<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /FlateDecode", flate(&checker));
  b.stream(
    65,
    "<< /Type /XObject /Subtype /Image /Width 8 /Height 1 /ColorSpace [/Indexed /DeviceRGB 1 <FF000000FF00>] /BitsPerComponent 1 /Filter /FlateDecode",
    flate(&idx_img),
  );
  b.stream(66, "<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode", flate(&smask));
  b.stream(67, "<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 66 0 R /Filter /FlateDecode", flate(&photo));
  b.stream(
    68,
    "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 1 /ImageMask true /Filter /FlateDecode",
    flate(&[0x80, 0x80]),
  );
  let mut p7 = Vec::new();
  p7.extend_from_slice(b"BT /F1 24 Tf 72 750 Td (Images) Tj ET\n");
  p7.extend_from_slice(b"q 64 0 0 64 72 650 cm /ImRGB Do Q\n");
  p7.extend_from_slice(b"q 32 0 0 32 200 650 cm /ImGray Do Q\n");
  p7.extend_from_slice(b"q 64 0 0 64 300 650 cm /ImIdx Do Q\n");
  p7.extend_from_slice(b"q 64 0 0 64 400 650 cm /ImPhoto Do Q\n");
  p7.extend_from_slice(b"0.8 0.2 0.2 rg q 24 0 0 24 72 560 cm /ImStencil Do Q\n");
  p7.extend_from_slice(b"q 32 0 0 32 200 560 cm BI /W 2 /H 2 /CS /RGB /BPC 8 ID \xFF\x00\x00\x00\xFF\x00\x00\x00\xFF\xFF\xFF\xFF EI Q\n");
  p7.extend_from_slice(b"BT /F1 12 Tf 72 520 Td (RGB, gray, indexed, SMask photo, stencil, inline) Tj ET\n");
  b.stream(26, "<< /Filter /FlateDecode", flate(&p7));
  b.raw(
    16,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 26 0 R /Resources << /Font << /F1 3 0 R >> /XObject << /ImRGB 63 0 R /ImGray 64 0 R /ImIdx 65 0 R /ImPhoto 67 0 R /ImStencil 68 0 R >> >> >>",
  );

  // Page 8: nested forms.
  let mut fb = Vec::new();
  fb.extend_from_slice(b"0.2 0.6 0.2 RG 2 w 0 0 100 60 re S\n");
  fb.extend_from_slice(b"BT /F1 12 Tf 10 25 Td (Form B) Tj ET\n");
  b.stream(
    71,
    "<< /Type /XObject /Subtype /Form /BBox [0 0 100 60] /Resources << /Font << /F1 3 0 R >> >> /Filter /FlateDecode",
    flate(&fb),
  );
  let mut fa = Vec::new();
  fa.extend_from_slice(b"q 1 0 0 1 0 70 cm /FmB Do Q\n");
  fa.extend_from_slice(b"0.8 0.1 0.1 rg 0 0 140 60 re f\n");
  fa.extend_from_slice(b"BT /F1 10 Tf 5 5 Td (Form A wraps B) Tj ET\n");
  b.stream(
    70,
    "<< /Type /XObject /Subtype /Form /BBox [0 0 140 140] /Resources << /Font << /F1 3 0 R >> /XObject << /FmB 71 0 R >> >> /Filter /FlateDecode",
    flate(&fa),
  );
  let mut p8 = Vec::new();
  p8.extend_from_slice(b"BT /F1 24 Tf 72 750 Td (Forms) Tj ET\n");
  p8.extend_from_slice(b"q 1 0 0 1 72 600 cm /FmA Do Q\n");
  p8.extend_from_slice(b"q 0.6 0 0 0.6 300 600 cm /FmA Do Q\n");
  b.stream(27, "<< /Filter /FlateDecode", flate(&p8));
  b.raw(
    17,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 27 0 R /Resources << /Font << /F1 3 0 R >> /XObject << /FmA 70 0 R >> >> >>",
  );

  // Page 9: annotations.
  let mut p9 = Vec::new();
  p9.extend_from_slice(b"BT /F1 24 Tf 72 750 Td (Annotations) Tj ET\n");
  p9.extend_from_slice(b"BT /F1 12 Tf 72 700 Td (Link to example.com, link to page 1, highlight, underline, square, circle, ink:) Tj ET\n");
  b.stream(28, "<< /Filter /FlateDecode", flate(&p9));
  b.raw(
    80,
    "<< /Type /Annot /Subtype /Link /Rect [72 660 250 680] /Border [0 0 1] /A << /S /URI /URI (https://example.com) >> >>",
  );
  b.raw(81, "<< /Type /Annot /Subtype /Link /Rect [72 630 250 650] /Border [0 0 0] /Dest [10 0 R /Fit] >>");
  b.raw(
    82,
    "<< /Type /Annot /Subtype /Highlight /Rect [72 600 300 620] /C [1 1 0] /QuadPoints [72 620 300 620 300 600 72 600] >>",
  );
  b.raw(
    83,
    "<< /Type /Annot /Subtype /Underline /Rect [72 570 300 590] /C [1 0 0] /QuadPoints [72 590 300 590 300 570 72 570] >>",
  );
  b.raw(84, "<< /Type /Annot /Subtype /Square /Rect [72 500 200 560] /C [0 0 1] /Border [0 0 2] >>");
  b.raw(85, "<< /Type /Annot /Subtype /Circle /Rect [230 500 360 560] /C [0 0.6 0] /Border [0 0 2] >>");
  b.raw(86, "<< /Type /Annot /Subtype /Ink /Rect [72 430 300 480] /C [0.5 0 0.5] /InkList [[72 470 120 450 170 470 220 445 290 465]] >>");
  b.raw(87, "<< /Type /Annot /Subtype /Text /Rect [72 400 92 420] /Contents (Sticky note) /C [1 1 0] >>");
  b.raw(
    18,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 28 0 R /Resources << /Font << /F1 3 0 R >> >> /Annots [80 0 R 81 0 R 82 0 R 83 0 R 84 0 R 85 0 R 86 0 R 87 0 R] >>",
  );

  // Page 10: mixed filters + CropBox + Rotate (known gap).
  let f1 = b"BT /F1 16 Tf 72 750 Td (Flate part) Tj ET\n".to_vec();
  let f2 = b"BT /F1 16 Tf 72 720 Td (ASCII85 part) Tj ET\n".to_vec();
  let f3 = b"BT /F1 16 Tf 72 690 Td (RunLength part) Tj ET\n".to_vec();
  let f4 = b"BT /F1 16 Tf 72 660 Td (LZW part with enough text to grow the table beyond nine bit codes a few times over) Tj ET\n".to_vec();
  b.stream(50, "<< /Filter /FlateDecode", flate(&f1));
  b.stream(51, "<< /Filter /ASCII85Decode", ascii85(&f2));
  b.stream(52, "<< /Filter /RunLengthDecode", runlength(&f3));
  b.stream(53, "<< /Filter /LZWDecode", lzw(&f4));
  b.raw(
    19,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /CropBox [72 400 540 792] /Rotate 90 /Contents [50 0 R 51 0 R 52 0 R 53 0 R] >>",
  );

  b.finish(1, 93)
}
