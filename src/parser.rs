use std::cell::RefCell;
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
  /// Resolved `/Resources` dict (inherited); `Null` when absent.
  /// Kept for ExtGState, XObject and color space lookups.
  pub resources: PdfValue,
}

/// Parses the file structure: header, xref table or stream, trailer
/// and objects (including objects packed in object streams).
pub struct FileParser {
  data: Vec<u8>,
  offsets: HashMap<u32, usize>,
  compressed: HashMap<u32, (u32, usize)>,
  objstm_cache: RefCell<HashMap<u32, Vec<(u32, PdfValue)>>>,
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
      compressed: HashMap::new(),
      objstm_cache: RefCell::new(HashMap::new()),
      trailer: PdfValue::Null,
    };
    parser.read_xref()?;
    if parser.offsets.is_empty() && parser.compressed.is_empty() {
      parser.scan_objects();
    }
    if parser.offsets.is_empty() && parser.compressed.is_empty() {
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
    let pos = self.skip_ws_at(offset);
    if self.data[pos..].starts_with(b"xref") {
      self.parse_table_at(pos + 4)?;
    } else {
      self.parse_xref_stream_at(pos)?;
    }
    Ok(())
  }

  /// Parse one classic xref table. Follows `trailer /Prev` so older
  /// sections load first and newer entries win.
  fn parse_table_at(&mut self, mut pos: usize) -> Result<()> {
    let mut used: Vec<(u32, usize)> = Vec::new();
    let mut trailer = PdfValue::Null;
    loop {
      pos = self.skip_ws_at(pos);
      if self.data[pos..].starts_with(b"trailer") {
        pos += 7;
        let mut p = ObjectParser::new(&self.data[pos..]);
        match p.parse_value()? {
          Some(PdfValue::Dict(_)) => {
            let mut reparsed = ObjectParser::new(&self.data[pos..]);
            trailer = reparsed.parse_value()?.unwrap_or(PdfValue::Null);
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
            used.push((num_u, off));
          }
        }
      }
    }
    if let Some(prev) = trailer.get("Prev").and_then(|v| v.as_number()) {
      if prev >= 0.0 {
        self.parse_table_at(prev as usize)?;
      }
    }
    for (num, off) in used {
      self.offsets.insert(num, off);
    }
    self.merge_trailer(&trailer);
    Ok(())
  }

  /// Parse an xref stream (`/Type /XRef`). Entries follow `/W` byte
  /// widths and `/Index`; `/Prev` chains load oldest-first.
  fn parse_xref_stream_at(&mut self, pos: usize) -> Result<()> {
    let mut p = ObjectParser::new(&self.data[pos..]);
    p.read_obj_header()?;
    let dict = match p.parse_value()? {
      Some(dict @ PdfValue::Dict(_)) => dict,
      _ => return Err(PdfError::InvalidObject("xref stream needs a dict".into())),
    };
    let after = p.offset();
    let raw = self.stream_body(&dict, &self.data[pos + after..])?
      .ok_or_else(|| PdfError::InvalidObject("xref stream without stream".into()))?;
    let bytes = crate::filter::decode(&dict, &raw)?;
    let widths = match dict.get("W").and_then(|v| v.as_array()) {
      Some(items) if items.len() == 3 => [
        items[0].as_number().unwrap_or(0.0) as usize,
        items[1].as_number().unwrap_or(0.0) as usize,
        items[2].as_number().unwrap_or(0.0) as usize,
      ],
      _ => return Err(PdfError::InvalidObject("xref stream needs /W".into())),
    };
    let size = dict.get("Size").and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
    let index: Vec<usize> = match dict.get("Index").and_then(|v| v.as_array()) {
      Some(items) => items.iter().filter_map(|v| v.as_number().map(|n| n as usize)).collect(),
      None => vec![0, size],
    };
    let stride: usize = widths.iter().sum();
    let mut entries: Vec<(usize, u64, u64, u64)> = Vec::new();
    let mut cursor = 0;
    let mut chunks = index.chunks(2);
    while let Some(pair) = chunks.next() {
      if pair.len() != 2 {
        return Err(PdfError::InvalidObject("bad xref /Index".into()));
      }
      for num in pair[0]..pair[0] + pair[1] {
        if cursor + stride > bytes.len() {
          return Err(PdfError::InvalidObject("truncated xref stream".into()));
        }
        let row = &bytes[cursor..cursor + stride];
        cursor += stride;
        let mut at = 0;
        let mut field = |w: usize| {
          let mut value = 0u64;
          for b in row.iter().skip(at).take(w) {
            value = (value << 8) | u64::from(*b);
          }
          at += w;
          value
        };
        let (t, f1, f2) = (field(widths[0]), field(widths[1]), field(widths[2]));
        let kind = if widths[0] == 0 { u64::from(num != 0) } else { t };
        entries.push((num, kind, f1, f2));
      }
    }
    if let Some(prev) = dict.get("Prev").and_then(|v| v.as_number()) {
      if prev >= 0.0 {
        let prev_pos = self.skip_ws_at(prev as usize);
        if self.data[prev_pos..].starts_with(b"xref") {
          self.parse_table_at(prev_pos + 4)?;
        } else {
          self.parse_xref_stream_at(prev_pos)?;
        }
      }
    }
    for (num, kind, f1, f2) in entries {
      let Ok(num_u) = u32::try_from(num) else { continue };
      match kind {
        0 => {
          self.offsets.remove(&num_u);
          self.compressed.remove(&num_u);
        }
        1 => {
          self.compressed.remove(&num_u);
          self.offsets.insert(num_u, f1 as usize);
        }
        2 => {
          self.offsets.remove(&num_u);
          if let (Ok(stm), Ok(idx)) = (u32::try_from(f1), usize::try_from(f2)) {
            self.compressed.insert(num_u, (stm, idx));
          }
        }
        _ => {}
      }
    }
    self.merge_trailer(&dict);
    Ok(())
  }

  /// Merge trailer keys (`/Root`, `/Info`, `/Encrypt`, `/ID`,
  /// `/Size`); newer sections win because older ones load first.
  fn merge_trailer(&mut self, newer: &PdfValue) {
    let entries = match newer {
      PdfValue::Dict(entries) => entries.clone(),
      _ => return,
    };
    if self.trailer == PdfValue::Null {
      self.trailer = PdfValue::Dict(vec![]);
    }
    if let PdfValue::Dict(current) = &mut self.trailer {
      for key in ["Root", "Info", "Encrypt", "ID", "Size"] {
        if let Some((_, value)) = entries.iter().find(|(k, _)| k == key) {
          if let Some(slot) = current.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value.clone();
          } else {
            current.push((key.into(), value.clone()));
          }
        }
      }
    }
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

  /// Read and parse the indirect object `num`, whether it lives at
  /// a file offset or packed inside an object stream.
  pub fn object(&self, num: u32) -> Result<IndirectObject> {
    if let Some(&offset) = self.offsets.get(&num) {
      return self.object_at(num, offset);
    }
    if let Some(&(stm, index)) = self.compressed.get(&num) {
      let packed = self.objstm_objects(stm)?;
      let (found, value) = packed.get(index).ok_or(PdfError::ObjectNotFound(num))?;
      if *found != num {
        return Err(PdfError::InvalidObject(format!("object stream {stm} index mismatch")));
      }
      return Ok(IndirectObject { num, gen: 0, value: value.clone(), stream: None });
    }
    Err(PdfError::ObjectNotFound(num))
  }

  /// Read an uncompressed object at a file offset.
  fn object_at(&self, num: u32, offset: usize) -> Result<IndirectObject> {
    let mut p = ObjectParser::new(&self.data[offset..]);
    let (obj_num, gen) = p.read_obj_header()?;
    let _ = obj_num;
    let value = match p.parse_value()? {
      Some(v) => v,
      None => return Err(PdfError::InvalidObject(format!("object {num} has no body"))),
    };
    let after = p.offset();
    let rest = &self.data[offset + after..];
    let stream = self.stream_body(&value, rest)?;
    Ok(IndirectObject { num, gen, value, stream })
  }

  /// Unpack an object stream (`/Type /ObjStm`) into its objects.
  /// Results are cached; returns `(object number, value)` in stream order.
  fn objstm_objects(&self, stm: u32) -> Result<Vec<(u32, PdfValue)>> {
    if let Some(cached) = self.objstm_cache.borrow().get(&stm) {
      return Ok(cached.clone());
    }
    let offset = self.offsets.get(&stm).copied().ok_or(PdfError::ObjectNotFound(stm))?;
    let obj = self.object_at(stm, offset)?;
    let raw = obj.stream.ok_or_else(|| PdfError::InvalidObject(format!("object {stm} is not a stream")))?;
    let bytes = crate::filter::decode(&obj.value, &raw)?;
    let count = obj.value.get("N").and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
    let first = obj.value.get("First").and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
    let mut header = ObjectParser::new(&bytes);
    let mut table: Vec<(u32, usize)> = Vec::with_capacity(count);
    for _ in 0..count {
      let num = match header.parse_value()? {
        Some(PdfValue::Number(n)) => n as u32,
        _ => return Err(PdfError::InvalidObject("bad ObjStm header".into())),
      };
      let off = match header.parse_value()? {
        Some(PdfValue::Number(n)) => n as usize,
        _ => return Err(PdfError::InvalidObject("bad ObjStm header".into())),
      };
      table.push((num, off));
    }
    let mut out = Vec::with_capacity(count);
    for (num, off) in table {
      let at = first + off;
      if at >= bytes.len() {
        return Err(PdfError::InvalidObject("bad ObjStm offset".into()));
      }
      let mut body = ObjectParser::new(&bytes[at..]);
      match body.parse_value()? {
        Some(value) => out.push((num, value)),
        None => return Err(PdfError::InvalidObject("empty ObjStm entry".into())),
      }
    }
    self.objstm_cache.borrow_mut().insert(stm, out.clone());
    Ok(out)
  }

  fn stream_body(&self, dict: &PdfValue, rest: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut i = 0;
    while i < rest.len() && rest[i].is_ascii_whitespace() {
      i += 1;
    }
    if i + 6 > rest.len() || &rest[i..i + 6] != b"stream" {
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
    if let Some(len_value) = dict.get("Length") {
      // Indirect lengths may not resolve yet while reading xref
      // streams; fall back to the endstream search then.
      if let Ok(resolved) = self.resolve(len_value) {
        if let Some(len) = resolved.as_number() {
          let declared = (len.max(0.0)) as usize;
          if declared <= length {
            length = declared;
          }
        }
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
      let resolved_res = self.resolve(resources).unwrap_or(PdfValue::Null);
      let fonts = self.page_fonts(&resolved_res)?;
      out.push(ParsedPage { index, media_box, content, fonts, resources: resolved_res });
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

  fn flate(raw: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(raw).unwrap();
    enc.finish().unwrap()
  }

  /// Build a fully compressed PDF: xref stream plus the font packed
  /// in an object stream. No classic xref table at all.
  fn compressed_pdf() -> Vec<u8> {
    let content = flate(b"BT /F1 12 Tf 72 720 Td (Packed) Tj ET");
    let objstm_inner = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>";
    let objstm_body = [b"5 0 ".to_vec(), objstm_inner.to_vec()].concat();
    let objstm_payload = flate(&objstm_body);
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.5\n");
    let mut offsets: Vec<usize> = Vec::new();
    let mut emit = |pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, num: u32, body: &[u8]| {
      offsets.push(pdf.len());
      pdf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
      pdf.extend_from_slice(body);
      pdf.extend_from_slice(b"\nendobj\n");
    };
    emit(&mut pdf, &mut offsets, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
    emit(&mut pdf, &mut offsets, 2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    emit(&mut pdf, &mut offsets, 3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    emit(
      &mut pdf,
      &mut offsets,
      4,
      &[b"<< /Length ".to_vec(), content.len().to_string().into_bytes(), b" /Filter /FlateDecode >>\nstream\n".to_vec(), content, b"\nendstream".to_vec()].concat(),
    );
    emit(
      &mut pdf,
      &mut offsets,
      6,
      &[
        b"<< /Type /ObjStm /N 1 /First 4 /Length ".to_vec(),
        objstm_payload.len().to_string().into_bytes(),
        b" /Filter /FlateDecode >>\nstream\n".to_vec(),
        objstm_payload,
        b"\nendstream".to_vec(),
      ]
      .concat(),
    );
    // xref stream object 7: entries for 0..7, W = [1, 4, 2].
    let xoff = pdf.len();
    let mut rows: Vec<u8> = Vec::new();
    let mut row = |t: u8, f1: u32, f2: u16| {
      rows.push(t);
      rows.extend_from_slice(&f1.to_be_bytes());
      rows.extend_from_slice(&f2.to_be_bytes());
    };
    row(0, 0, 65535);
    for off in &offsets[..4] {
      row(1, *off as u32, 0);
    }
    row(2, 6, 0);
    row(1, offsets[4] as u32, 0);
    row(1, xoff as u32, 0);
    let xpayload = flate(&rows);
    pdf.extend_from_slice(
      &[
        b"7 0 obj\n<< /Type /XRef /Size 8 /Root 1 0 R /W [1 4 2] /Length ".to_vec(),
        xpayload.len().to_string().into_bytes(),
        b" /Filter /FlateDecode >>\nstream\n".to_vec(),
        xpayload,
        b"\nendstream\nendobj\n".to_vec(),
      ]
      .concat(),
    );
    pdf.extend_from_slice(b"startxref\n");
    pdf.extend_from_slice(xoff.to_string().as_bytes());
    pdf.extend_from_slice(b"\n%%EOF");
    pdf
  }

  #[test]
  fn reads_xref_stream_and_objstm() {
    let parser = FileParser::new(compressed_pdf()).unwrap();
    let pages = parser.pages().unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].fonts.len(), 1);
    assert_eq!(pages[0].fonts[0].base_font, "Helvetica-Bold");
    assert!(pages[0].content.windows(6).any(|w| w == b"Packed"));
  }

  #[test]
  fn reads_predicted_xref_stream() {
    // Predictor decoding on xref-style rows (DecodeParms path).
    let mut rows: Vec<u8> = Vec::new();
    let mut row = |t: u8, f1: u32, f2: u16| {
      rows.push(t);
      rows.extend_from_slice(&f1.to_be_bytes());
      rows.extend_from_slice(&f2.to_be_bytes());
    };
    row(0, 0, 65535);
    row(1, 9, 0);
    row(1, 60, 0);
    // Encode with Up filter per row (stride 7).
    let mut stored = Vec::new();
    let mut prev = [0u8; 7];
    for chunk in rows.chunks(7) {
      stored.push(2);
      for (i, &b) in chunk.iter().enumerate() {
        stored.push(b.wrapping_sub(prev[i]));
      }
      prev.copy_from_slice(chunk);
    }
    let payload = flate(&stored);
    let dict = PdfValue::Dict(vec![
      ("Filter".into(), PdfValue::Name("FlateDecode".into())),
      (
        "DecodeParms".into(),
        PdfValue::Dict(vec![
          ("Predictor".into(), PdfValue::Number(12.0)),
          ("Columns".into(), PdfValue::Number(7.0)),
        ]),
      ),
    ]);
    let decoded = crate::filter::decode(&dict, &payload).unwrap();
    assert_eq!(decoded, rows);
  }

  #[test]
  fn follows_prev_chain() {
    // Base file plus an incremental update that adds a new font object.
    let mut pdf = minimal_pdf(b"BT /F1 12 Tf (v1) Tj ET");
    let cut = pdf.windows(9).rposition(|w| w == b"startxref").unwrap();
    pdf.truncate(cut);
    let new_font = b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>";
    let f6_off = pdf.len();
    pdf.extend_from_slice(b"6 0 obj\n");
    pdf.extend_from_slice(new_font);
    pdf.extend_from_slice(b"\nendobj\n");
    let table_off = pdf.len();
    pdf.extend_from_slice(b"xref\n6 1\n");
    pdf.extend_from_slice(format!("{f6_off:010} 00000 n \n").as_bytes());
    let first_xref = pdf.windows(4).position(|w| w == b"xref").unwrap();
    pdf.extend_from_slice(b"trailer\n");
    pdf.extend_from_slice(format!("<< /Size 7 /Root 1 0 R /Prev {first_xref} >>\n").as_bytes());
    pdf.extend_from_slice(b"startxref\n");
    pdf.extend_from_slice(table_off.to_string().as_bytes());
    pdf.extend_from_slice(b"\n%%EOF");
    let parser = FileParser::new(pdf).unwrap();
    let font = parser.object(6).unwrap();
    assert_eq!(font.value.get("BaseFont").and_then(|v| v.as_name()), Some("Courier"));
    assert!(parser.pages().is_ok());
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
