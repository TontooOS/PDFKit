use std::collections::HashMap;

use crate::error::{PdfError, Result};
use crate::objects::{ObjectParser, PdfValue};

/// Indirect object header plus parsed body value.
#[derive(Debug, Clone)]
pub struct IndirectObject {
  /// Object number.
  pub num: u32,
  /// Generation number.
  pub gen: u16,
  /// Body value (dictionary for streams).
  pub value: PdfValue,
  /// Raw stream bytes when the object is a stream, `None` otherwise.
  pub stream: Option<Vec<u8>>,
}

/// Font reference resolved from a page `/Resources` dictionary.
#[derive(Debug, Clone)]
pub struct PageFont {
  /// Resource name without slash, e.g. `F1`.
  pub resource: String,
  /// `/BaseFont` name, e.g. `Helvetica-Bold`.
  pub base_font: String,
}

/// One page found by walking the page tree.
#[derive(Debug, Clone)]
pub struct ParsedPage {
  /// Zero-based page index in document order.
  pub index: usize,
  /// `/MediaBox` in points `[x0, y0, x1, y1]`.
  pub media_box: [f32; 4],
  /// Decoded content stream bytes (all `/Contents` concatenated).
  pub content: Vec<u8>,
  /// Fonts declared in the page resources.
  pub fonts: Vec<PageFont>,
}

/// Parses the file structure: header, xref table, trailer and objects.
pub struct FileParser {
  data: Vec<u8>,
  offsets: HashMap<u32, usize>,
  trailer: PdfValue,
}

impl FileParser {
  /// Parse the structure of `data`. Content streams stay encoded
  /// until a page is resolved.
  pub fn new(data: Vec<u8>) -> Result<Self> {
    if data.is_empty() {
      return Err(PdfError::Empty);
    }
    if !data.starts_with(b"%PDF-") {
      return Err(PdfError::InvalidHeader);
    }
    let mut parser = Self {
      data,
      offsets: HashMap::new(),
      trailer: PdfValue::Null,
    };
    parser.read_xref()?;
    if parser.offsets.is_empty() {
      parser.scan_objects();
    }
    if parser.offsets.is_empty() {
      return Err(PdfError::XrefNotFound);
    }
    Ok(parser)
  }

  fn tail_text(&self, len: usize) -> &[u8] {
    let start = self.data.len().saturating_sub(len);
    &self.data[start..]
  }

  fn find_startxref(&self) -> Option<usize> {
    let tail = self.tail_text(2048);
    let marker = b"startxref";
    let rel = tail.windows(marker.len()).rposition(|w| w == marker)?;
    let mut pos = rel + marker.len();
    while pos < tail.len() && tail[pos].is_ascii_whitespace() {
      pos += 1;
    }
    let start = pos;
    while pos < tail.len() && tail[pos].is_ascii_digit() {
      pos += 1;
    }
    let abs = self.data.len().saturating_sub(tail.len()) + start;
    std::str::from_utf8(&self.data[abs..abs + (pos - start)])
      .ok()?
      .parse::<usize>()
      .ok()
  }

  fn read_xref(&mut self) -> Result<()> {
    let offset = match self.find_startxref() {
      Some(offset) => offset,
      None => return Ok(()),
    };
    if self.data.get(offset..offset + 4) != Some(b"xref".as_slice()) {
      return Ok(());
    }
    let mut pos = offset + 4;
    loop {
      pos = self.skip_ws_at(pos);
      if self.data[pos..].starts_with(b"trailer") {
        pos += 7;
        let mut p = ObjectParser::new(&self.data[pos..]);
        match p.parse_value()? {
          Some(PdfValue::Dict(_)) => {
            let full = ObjectParser::new(&self.data[pos..]);
            let _ = full;
            let mut reparsed = ObjectParser::new(&self.data[pos..]);
            self.trailer = reparsed.parse_value()?.unwrap_or(PdfValue::Null);
          }
          _ => return Err(PdfError::InvalidObject("trailer must be a dict".into())),
        }
        break;
      }
      let head_end = self.line_end(pos);
      let head = String::from_utf8_lossy(&self.data[pos..head_end]).into_owned();
      let mut parts = head.split_whitespace();
      let first: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
      let count: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
      if first == usize::MAX {
        break;
      }
      pos = head_end;
      for num in first..first + count {
        pos = self.skip_ws_at(pos);
        let end = self.line_end(pos);
        let line = String::from_utf8_lossy(&self.data[pos..end]).into_owned();
        pos = end;
        let mut fields = line.split_whitespace();
        let off: usize = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let in_use = fields.next().unwrap_or("f") == "n";
        if in_use {
          if let Ok(num_u) = u32::try_from(num) {
            self.offsets.insert(num_u, off);
          }
        }
      }
    }
    Ok(())
  }

  fn skip_ws_at(&self, mut pos: usize) -> usize {
    while pos < self.data.len() && (self.data[pos].is_ascii_whitespace() || self.data[pos] == 0) {
      pos += 1;
    }
    pos
  }

  fn line_end(&self, pos: usize) -> usize {
    let mut end = pos;
    while end < self.data.len() && self.data[end] != b'\n' {
      end += 1;
    }
    if end < self.data.len() {
      end += 1;
    }
    end
  }

  /// Fallback for files without a usable xref table: scan every
  /// `N G obj` header in the file.
  fn scan_objects(&mut self) {
    let mut i = 0;
    while i + 8 < self.data.len() {
      if self.data[i].is_ascii_digit() {
        let start = i;
        while i < self.data.len() && self.data[i].is_ascii_digit() {
          i += 1;
        }
        let num_ok = i < self.data.len() && self.data[i] == b' ';
        let mut j = i + 1;
        while j < self.data.len() && self.data[j].is_ascii_digit() {
          j += 1;
        }
        if num_ok && self.data[j..].starts_with(b" obj") {
          if let Ok(num) = std::str::from_utf8(&self.data[start..i]).unwrap_or("").parse::<u32>() {
            self.offsets.entry(num).or_insert(start);
          }
          i = j;
          continue;
        }
      }
      i += 1;
    }
    if self.trailer == PdfValue::Null {
      self.trailer = PdfValue::Dict(vec![("Size".into(), PdfValue::Number(self.offsets.len() as f64))]);
    }
  }

  /// Read and parse the indirect object `num`.
  pub fn object(&self, num: u32) -> Result<IndirectObject> {
    let offset = self.offsets.get(&num).copied().ok_or(PdfError::ObjectNotFound(num))?;
    let mut p = ObjectParser::new(&self.data[offset..]);
    let (obj_num, gen) = p.read_obj_header()?;
    let _ = obj_num;
    let value = match p.parse_value()? {
      Some(v) => v,
      None => return Err(PdfError::InvalidObject(format!("object {num} has no body"))),
    };
    let after = p.offset();
    let rest = &self.data[offset + after..];
    let stream = Self::stream_body(&value, rest)?;
    Ok(IndirectObject { num, gen, value, stream })
  }

  fn stream_body(dict: &PdfValue, rest: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut i = 0;
    while i < rest.len() && rest[i].is_ascii_whitespace() {
      i += 1;
    }
    if !rest[i..].starts_with(b"stream") {
      return Ok(None);
    }
    i += 6;
    if rest[i..].starts_with(b"\r\n") {
      i += 2;
    } else if rest.get(i) == Some(&b'\r') || rest.get(i) == Some(&b'\n') {
      i += 1;
    }
    let end = find_bytes(rest, b"endstream").ok_or_else(|| PdfError::InvalidObject("stream without endstream".into()))?;
    let mut length = end.saturating_sub(i);
    if let Some(PdfValue::Number(len)) = dict.get("Length") {
      let declared = (*len).max(0.0) as usize;
      if declared <= length {
        length = declared;
      }
    }
    Ok(Some(rest[i..i + length].to_vec()))
  }

  /// Resolve an indirect reference to its body value.
  pub fn resolve(&self, value: &PdfValue) -> Result<PdfValue> {
    match value {
      PdfValue::Ref(num, _) => Ok(self.object(*num)?.value),
      other => Ok(other.clone()),
    }
  }

  fn resolve_ref(&self, value: &PdfValue) -> Result<Option<(u32, PdfValue)>> {
    match value {
      PdfValue::Ref(num, _) => {
        let obj = self.object(*num)?;
        Ok(Some((*num, obj.value)))
      }
      _ => Ok(None),
    }
  }

  /// Walk the page tree and return every page with decoded content.
  pub fn pages(&self) -> Result<Vec<ParsedPage>> {
    let root = self.trailer.get("Root").ok_or_else(|| PdfError::InvalidObject("trailer has no /Root".into()))?;
    let root_num = root.as_ref().map(|(n, _)| n).ok_or_else(|| PdfError::InvalidObject("/Root is not a reference".into()))?;
    let root_obj = self.object(root_num)?;
    let pages_ref = root_obj
      .value
      .get("Pages")
      .ok_or_else(|| PdfError::InvalidObject("catalog has no /Pages".into()))?;
    let pages_num = pages_ref.as_ref().map(|(n, _)| n).ok_or_else(|| PdfError::InvalidObject("/Pages is not a reference".into()))?;
    let mut out = Vec::new();
    self.walk_pages(pages_num, &PdfValue::Null, &PdfValue::Null, &mut out)?;
    if out.is_empty() {
      return Err(PdfError::NoPages);
    }
    Ok(out)
  }

  fn walk_pages(
    &self,
    node_num: u32,
    inherited_box: &PdfValue,
    inherited_res: &PdfValue,
    out: &mut Vec<ParsedPage>,
  ) -> Result<()> {
    let node = self.object(node_num)?;
    let kind = node.value.get("Type").and_then(|v| v.as_name()).unwrap_or("");
    let media = node.value.get("MediaBox").unwrap_or(inherited_box);
    let resources = node.value.get("Resources").unwrap_or(inherited_res);
    if kind == "Page" {
      let index = out.len();
      let media_box = Self::media_box(media);
      let content = self.page_content(&node.value)?;
      let fonts = self.page_fonts(resources)?;
      out.push(ParsedPage { index, media_box, content, fonts });
      return Ok(());
    }
    let kids = node
      .value
      .get("Kids")
      .and_then(|v| v.as_array())
      .ok_or_else(|| PdfError::InvalidObject("pages node has no /Kids".into()))?;
    for kid in kids {
      if let Some((num, _)) = kid.as_ref() {
        self.walk_pages(num, media, resources, out)?;
      }
    }
    Ok(())
  }

  fn media_box(value: &PdfValue) -> [f32; 4] {
    let mut box_vals = [0.0, 0.0, 612.0, 792.0];
    if let Some(items) = value.as_array() {
      for (i, slot) in box_vals.iter_mut().enumerate() {
        if let Some(n) = items.get(i).and_then(|v| v.as_number()) {
          *slot = n as f32;
        }
      }
    }
    box_vals
  }

  fn page_content(&self, page: &PdfValue) -> Result<Vec<u8>> {
    let contents = page.get("Contents");
    let mut out = Vec::new();
    match contents {
      None => Ok(out),
      Some(PdfValue::Ref(num, _)) => {
        out.extend(self.decoded_stream(*num)?);
        Ok(out)
      }
      Some(PdfValue::Array(items)) => {
        for item in items {
          if let Some((num, _)) = item.as_ref() {
            let bytes = self.decoded_stream(num)?;
            out.extend(bytes);
            out.push(b'\n');
          }
        }
        Ok(out)
      }
      Some(other) => {
        let resolved = self.resolve(other)?;
        match resolved {
          PdfValue::Ref(num, _) => {
            out.extend(self.decoded_stream(num)?);
            Ok(out)
          }
          _ => Err(PdfError::InvalidObject("/Contents must be a stream reference".into())),
        }
      }
    }
  }

  fn decoded_stream(&self, num: u32) -> Result<Vec<u8>> {
    let obj = self.object(num)?;
    let raw = obj.stream.ok_or_else(|| PdfError::InvalidObject(format!("object {num} is not a stream")))?;
    Self::decode_stream(&obj.value, &raw)
  }

  /// Decode a stream body honoring `/Filter` and `/DecodeParms`
  /// (see the `filter` module for the supported set).
  pub fn decode_stream(dict: &PdfValue, raw: &[u8]) -> Result<Vec<u8>> {
    crate::filter::decode(dict, raw)
  }

  /// Resolve a page `/Resources` value into its font list.
  fn page_fonts(&self, resources: &PdfValue) -> Result<Vec<PageFont>> {
    let resolved = self.resolve(resources)?;
    let fonts_dict = match resolved.get("Font") {
      Some(PdfValue::Ref(num, _)) => self.object(*num)?.value,
      Some(other) => self.resolve(other)?,
      None => return Ok(vec![]),
    };
    let entries = match &fonts_dict {
      PdfValue::Dict(entries) => entries.clone(),
      _ => return Ok(vec![]),
    };
    let mut fonts = Vec::new();
    for (resource, value) in &entries {
      let font_num = match value {
        PdfValue::Ref(num, _) => *num,
        _ => continue,
      };
      let font_obj = self.object(font_num)?;
      let base = font_obj
        .value
        .get("BaseFont")
        .and_then(|v| v.as_name())
        .unwrap_or("Unknown")
        .to_owned();
      fonts.push(PageFont { resource: resource.clone(), base_font: base });
    }
    Ok(fonts)
  }

  /// Number of indirect objects found (structure info for tests).
  pub fn object_count(&self) -> usize {
    self.offsets.len()
  }

  /// Access a raw indirect object (used by tests and future features).
  pub fn raw_object(&self, num: u32) -> Result<IndirectObject> {
    self.object(num)
  }

  /// Resolve helper kept for document-level lookups.
  pub fn resolve_value(&self, value: &PdfValue) -> Result<PdfValue> {
    self.resolve(value)
  }

  /// Direct reference lookup without cloning the whole value.
  pub fn lookup(&self, value: &PdfValue) -> Result<Option<(u32, PdfValue)>> {
    self.resolve_ref(value)
  }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
  haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;
  use std::io::Write;

  pub fn minimal_pdf(content: &[u8]) -> Vec<u8> {
    let mut pdf = Vec::new();
    let mut offsets = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let objects: Vec<Vec<u8>> = vec![
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
      [b"<< /Length ".to_vec(), content.len().to_string().into_bytes(), b" >>\nstream\n".to_vec(), content.to_vec(), b"\nendstream".to_vec()].concat(),
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    for (i, body) in objects.iter().enumerate() {
      offsets.push(pdf.len());
      pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
      pdf.extend_from_slice(body);
      pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
      pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n");
    pdf.extend_from_slice(xref.to_string().as_bytes());
    pdf.extend_from_slice(b"\n%%EOF");
    pdf
  }

  #[test]
  fn walks_single_page() {
    let pdf = minimal_pdf(b"BT /F1 12 Tf 72 720 Td (Hello) Tj ET");
    let parser = FileParser::new(pdf).unwrap();
    let pages = parser.pages().unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].media_box, [0.0, 0.0, 612.0, 792.0]);
    assert_eq!(pages[0].fonts.len(), 1);
    assert_eq!(pages[0].fonts[0].base_font, "Helvetica");
  }

  #[test]
  fn decodes_flate_stream() {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(b"BT /F1 12 Tf 72 720 Td (Hi) Tj ET").unwrap();
    let compressed = enc.finish().unwrap();
    let mut pdf = minimal_pdf(&[]);
    let _ = pdf;
    let inner: Vec<u8> = [
      b"<< /Length ".to_vec(),
      compressed.len().to_string().into_bytes(),
      b" /Filter /FlateDecode >>\nstream\n".to_vec(),
      compressed,
      b"\nendstream".to_vec(),
    ]
    .concat();
    let pdf = minimal_pdf_full(inner);
    let parser = FileParser::new(pdf).unwrap();
    let pages = parser.pages().unwrap();
    assert!(pages[0].content.windows(2).any(|w| w == b"Hi"));
  }

  fn minimal_pdf_full(stream_obj: Vec<u8>) -> Vec<u8> {
    let mut pdf = Vec::new();
    let mut offsets = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let objects: Vec<Vec<u8>> = vec![
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
      stream_obj,
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    for (i, body) in objects.iter().enumerate() {
      offsets.push(pdf.len());
      pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
      pdf.extend_from_slice(body);
      pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
      pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n");
    pdf.extend_from_slice(xref.to_string().as_bytes());
    pdf.extend_from_slice(b"\n%%EOF");
    pdf
  }
}
