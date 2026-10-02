//! Incremental update writer: appends edited objects to an existing
//! PDF instead of rewriting it.
//!
//! ISO 32000 section 7.5.6 lets a file be updated by appending the
//! changed objects plus a cross-reference section whose `/Prev` points
//! at the previous one. The original bytes stay byte-for-byte intact,
//! so anything this writer does not model (object types, key
//! structures, producer hints) survives untouched. A full rewrite would
//! have to serialize every object again and would lose whatever the
//! parser does not understand.
//!
//! The appended section mirrors the original's flavor: a file that used
//! an xref table gets a table, a file that used an xref stream gets a
//! stream. Mixing the two is what hybrid-reference files do and readers
//! disagree about it.

use crate::error::Result;
use crate::filter::deflate;
use crate::objects::PdfValue;

/// Trailer entries the writer must carry forward.
#[derive(Debug, Clone, Default)]
pub struct Trailer {
  /// `/Root` catalog reference.
  pub root: Option<(u32, u16)>,
  /// `/Info` metadata reference.
  pub info: Option<(u32, u16)>,
  /// `/Encrypt` reference; present means the file is encrypted.
  pub encrypt: Option<(u32, u16)>,
  /// `/ID` first element (the second is written equal to it).
  pub id: Option<Vec<u8>>,
  /// `/Size` from the previous trailer.
  pub size: u32,
}

/// One appended indirect object.
struct Appended {
  num: u32,
  gen: u16,
  body: Vec<u8>,
}

/// Builder for an incremental update.
pub struct UpdateWriter {
  original: Vec<u8>,
  /// `startxref` offset of the previous section.
  prev_startxref: u64,
  /// True when the previous section was an xref stream.
  prev_is_stream: bool,
  trailer: Trailer,
  appended: Vec<Appended>,
  next_num: u32,
}

impl UpdateWriter {
  /// Start an update that appends to `original`.
  pub fn new(original: Vec<u8>, prev_startxref: u64, prev_is_stream: bool, trailer: Trailer, next_num: u32) -> Self {
    Self { original, prev_startxref, prev_is_stream, trailer, appended: Vec::new(), next_num }
  }

  /// Next free object number.
  pub fn next_num(&self) -> u32 {
    self.next_num
  }

  /// Append a plain object given as raw body bytes (no `N G obj`
  /// wrapper, no `endobj`).
  pub fn push_raw(&mut self, num: u32, gen: u16, body: Vec<u8>) {
    self.appended.push(Appended { num, gen, body });
    self.next_num = self.next_num.max(num + 1);
  }

  /// Append a Flate-compressed stream object at a fresh object
  /// number and return that number.
  pub fn push_stream(&mut self, dict_entries: &[(&str, String)], data: &[u8]) -> u32 {
    let num = self.next_num;
    let compressed = deflate(data);
    let mut body = String::from("<< ");
    for (key, value) in dict_entries {
      body.push('/');
      body.push_str(key);
      body.push(' ');
      body.push_str(value);
      body.push(' ');
    }
    body.push_str("/Filter /FlateDecode /Length ");
    body.push_str(&compressed.len().to_string());
    body.push_str(" >>\nstream\n");
    let mut bytes = body.into_bytes();
    bytes.extend_from_slice(&compressed);
    bytes.extend_from_slice(b"\nendstream");
    self.push_raw(num, 0, bytes);
    num
  }

  /// Serialize a `PdfValue` as an object body (no wrapper).
  pub fn encode_value(value: &PdfValue) -> Vec<u8> {
    let mut out = String::new();
    encode_into(value, &mut out);
    out.into_bytes()
  }

  /// `/Size` for the new trailer: past every object this update wrote
  /// and past the original's own size.
  ///
  /// Takes the entries explicitly because `finish` moves the appended
  /// list out of `self` before writing the section.
  fn new_size(&self, entries: &[(u32, u64, u16)]) -> u32 {
    match entries.iter().map(|(num, _, _)| *num).max() {
      Some(max) => self.trailer.size.max(max + 1),
      None => self.trailer.size,
    }
  }

  /// Append the changed objects plus a new cross-reference section.
  ///
  /// Objects are written in ascending number order, which keeps the
  /// section predictable for readers and for diffing.
  pub fn finish(mut self) -> Result<Vec<u8>> {
    if self.appended.is_empty() {
      return Ok(self.original);
    }
    self.appended.sort_by_key(|a| a.num);

    // Move the buffer and the object list out, so `self` stays whole
    // and the section writers below can still read the trailer.
    let prev_is_stream = self.prev_is_stream;
    let mut original = std::mem::take(&mut self.original);
    let appended = std::mem::take(&mut self.appended);
    // Ensure the previous file ends on a line so appended objects do
    // not run into trailing junk.
    if !original.ends_with(b"\n") {
      original.push(b'\n');
    }
    let mut out = original;

    // Offsets are absolute in the new file, so record while writing.
    let mut entries: Vec<(u32, u64, u16)> = Vec::with_capacity(appended.len());
    for object in &appended {
      entries.push((object.num, out.len() as u64, object.gen));
      out.extend_from_slice(format!("{} {} obj\n", object.num, object.gen).as_bytes());
      out.extend_from_slice(&object.body);
      out.extend_from_slice(b"\nendobj\n");
    }

    if prev_is_stream {
      self.write_xref_stream(&mut out, &mut entries);
    } else {
      let xref_offset = out.len() as u64;
      let size = self.new_size(&entries);
      self.write_xref_table(&mut out, &entries, xref_offset, size);
    }
    Ok(out)
  }

  /// Classic cross-reference table plus a trailer with `/Prev`.
///
/// One subsection per run of consecutive object numbers. Gaps between
/// runs are left out entirely rather than listed as free: an
/// incremental update only describes the objects it wrote, and marking
/// an untouched object free in the newest section would make readers
/// drop it (newest section wins).
fn write_xref_table(&self, out: &mut Vec<u8>, entries: &[(u32, u64, u16)], xref_offset: u64, size: u32) {
    out.extend_from_slice(b"xref\n");
    for run in contiguous_runs(entries) {
      let first = run[0].0;
      let last = run[run.len() - 1].0;
      out.extend_from_slice(format!("{first} {}\n", last - first + 1).as_bytes());
      for (_, offset, _) in run {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
      }
    }

    out.extend_from_slice(b"trailer\n<< ");
    self.write_trailer_body(out, size);
    out.extend_from_slice(b" >>\nstartxref\n");
    out.extend_from_slice(xref_offset.to_string().as_bytes());
    out.extend_from_slice(b"\n%%EOF\n");
  }

  /// Cross-reference stream (`/Type /XRef`, `/W [1 4 2]`).
  ///
  /// The stream must describe itself, so it gets a fresh object number
  /// and its own entry - which is why `entries` is mutable here and why
  /// the object's offset is known before it is written.
  fn write_xref_stream(&self, out: &mut Vec<u8>, entries: &mut Vec<(u32, u64, u16)>) {
    let xref_num = self.next_num;
    let xref_offset = out.len() as u64;
    entries.push((xref_num, xref_offset, 0));
    entries.sort_by_key(|(num, _, _)| *num);

    let size = xref_num + 1;
    let size = self.new_size(entries).max(size);

    // /W [1 4 2]: type byte, 4-byte offset, 2-byte generation. Only the
    // objects this update wrote get an entry; untouched objects are
    // resolved through /Prev, so gaps must not be invented here.
    let runs = contiguous_runs(entries);
    let mut index = String::new();
    let mut data = Vec::with_capacity(entries.len() * 7);
    for run in runs.iter() {
      if !index.is_empty() {
        index.push(' ');
      }
      let first = run[0].0;
      index.push_str(&format!("{} {}", first, run.len()));
      for (_, offset, gen) in run.iter() {
        data.push(1);
        data.extend_from_slice(&(*offset as u32).to_be_bytes());
        data.extend_from_slice(&gen.to_be_bytes());
      }
    }

    let compressed = deflate(&data);
    out.extend_from_slice(format!("{xref_num} 0 obj\n").as_bytes());
    out.extend_from_slice(b"<< /Type /XRef /Size ");
    out.extend_from_slice(size.to_string().as_bytes());
    out.extend_from_slice(b" /Index [");
    out.extend_from_slice(index.as_bytes());
    out.extend_from_slice(b"] /W [1 4 2] /Root ");
    match self.trailer.root {
      Some((n, g)) => out.extend_from_slice(format!("{n} {g} R").as_bytes()),
      None => out.extend_from_slice(b"1 0 R"),
    }
    if let Some((n, g)) = self.trailer.info {
      out.extend_from_slice(format!(" /Info {n} {g} R").as_bytes());
    }
    if let Some((n, g)) = self.trailer.encrypt {
      out.extend_from_slice(format!(" /Encrypt {n} {g} R").as_bytes());
    }
    if let Some(id) = &self.trailer.id {
      out.extend_from_slice(format!(" /ID [<{}> <{}>]", hex_string(id), hex_string(id)).as_bytes());
    }
    out.extend_from_slice(format!(" /Prev {}", self.prev_startxref).as_bytes());
    out.extend_from_slice(b" /Filter /FlateDecode /Length ");
    out.extend_from_slice(compressed.len().to_string().as_bytes());
    out.extend_from_slice(b" >>\nstream\n");
    out.extend_from_slice(&compressed);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    out.extend_from_slice(b"startxref\n");
    out.extend_from_slice(xref_offset.to_string().as_bytes());
    out.extend_from_slice(b"\n%%EOF\n");
  }

  /// Shared trailer entries, including `/Prev`.
  fn write_trailer_body(&self, out: &mut Vec<u8>, size: u32) {
    out.extend_from_slice(format!("/Size {size}").as_bytes());
    match self.trailer.root {
      Some((n, g)) => out.extend_from_slice(format!(" /Root {n} {g} R").as_bytes()),
      None => out.extend_from_slice(b" /Root 1 0 R"),
    }
    if let Some((n, g)) = self.trailer.info {
      out.extend_from_slice(format!(" /Info {n} {g} R").as_bytes());
    }
    // `/Encrypt` is carried forward so the appended objects would be
    // covered by the same encryption. Editing encrypted files is
    // refused before this point, because the new bytes are plaintext.
    if let Some((n, g)) = self.trailer.encrypt {
      out.extend_from_slice(format!(" /Encrypt {n} {g} R").as_bytes());
    }
    if let Some(id) = &self.trailer.id {
      out.extend_from_slice(format!(" /ID [<{}> <{}>]", hex_string(id), hex_string(id)).as_bytes());
    }
    out.extend_from_slice(format!(" /Prev {}", self.prev_startxref).as_bytes());
  }
}

/// Split ascending entries into runs of consecutive object numbers.
///
/// A cross-reference section describes a set of objects, not a range
/// with holes. Emitting one subsection (or one `/Index` pair) per run
/// keeps untouched objects out of the newest section instead of
/// claiming them as free, which readers would honour and lose the
/// original objects.
fn contiguous_runs(entries: &[(u32, u64, u16)]) -> Vec<&[(u32, u64, u16)]> {
  let mut runs: Vec<&[(u32, u64, u16)]> = Vec::new();
  let mut start = 0usize;
  for i in 1..=entries.len() {
    let breaks = i == entries.len() || entries[i].0 != entries[i - 1].0 + 1;
    if breaks {
      runs.push(&entries[start..i]);
      start = i;
    }
  }
  runs
}

/// Hex-encode bytes for an `/ID` string.
fn hex_string(bytes: &[u8]) -> String {
  let mut out = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    out.push_str(&format!("{byte:02X}"));
  }
  out
}

/// Write a `PdfValue` in PDF syntax.
///
/// Every element inside an array or dictionary is preceded by a space.
/// Without it `/Parent` followed by `2 0 R` would merge into the single
/// name `Parent2`, which is exactly the kind of corruption a reader
/// silently accepts until it cannot find the object.
fn encode_into(value: &PdfValue, out: &mut String) {
  match value {
    PdfValue::Null => out.push_str("null"),
    PdfValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
    PdfValue::Number(n) => {
      if n.is_finite() && *n == n.trunc() && n.abs() < 1e15 {
        out.push_str(&format!("{}", *n as i64));
      } else {
        out.push_str(&format!("{n}"));
      }
    }
    PdfValue::Str(bytes) => {
      out.push('(');
      for &byte in bytes {
        match byte {
          b'(' | b')' | b'\\' => {
            out.push('\\');
            out.push(byte as char);
          }
          b'\r' => out.push_str("\\r"),
          b'\n' => out.push_str("\\n"),
          0x20..=0x7e => out.push(byte as char),
          _ => out.push_str(&format!("\\{byte:03o}")),
        }
      }
      out.push(')');
    }
    PdfValue::Hex(bytes) => {
      out.push('<');
      for byte in bytes {
        out.push_str(&format!("{byte:02X}"));
      }
      out.push('>');
    }
    PdfValue::Name(name) => {
      out.push('/');
      for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '+') {
          out.push(ch);
        } else {
          out.push_str(&format!("#{:02X}", ch as u32));
        }
      }
    }
    PdfValue::Ref(num, gen) => out.push_str(&format!("{num} {gen} R")),
    PdfValue::Array(items) => {
      out.push('[');
      for item in items {
        out.push(' ');
        encode_into(item, out);
      }
      out.push(']');
    }
    PdfValue::Dict(entries) => {
      out.push_str("<<");
      for (key, item) in entries {
        // Space before the key and after it: `/Parent` followed by
        // `2 0 R` without a separator merges into the name `Parent2`.
        out.push_str(" /");
        out.push_str(key);
        out.push(' ');
        encode_into(item, out);
      }
      out.push_str(">>");
    }
  }
}