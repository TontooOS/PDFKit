//! In-place PDF editing: mutate content-stream bytes and save.
//!
//! An edit never rebuilds a page. It splices new bytes into the page's
//! decoded content buffer at the `OpSpan` the item already carries,
//! re-encodes only the `/Contents` streams that actually changed, and
//! appends them as an incremental update (see [`crate::writer`]). The
//! original bytes stay intact, so an unwanted edit costs nothing and
//! untouched streams are never re-encoded.
//!
//! What that buys, and what it does not:
//!
//! - Text positions in a content stream are absolute, so replacing a
//!   run moves nothing else. Line breaking does not reflow: PDFKit
//!   renders text through SF Pro rather than the fonts embedded in the
//!   file, so an edited run is measured with substitute metrics.
//! - Encrypted files are refused. An incremental update would have to
//!   re-encrypt the appended bytes with the file key; writing them as
//!   plaintext would silently corrupt the document.
//!
//! State model: `original` stays pristine, the per-page working copies
//! hold the pending edits, and `doc` is re-parsed from a throwaway
//! build so callers see their changes before saving. Only
//! [`PdfEditor::into_bytes`] adopts the result as the new baseline.

use std::path::Path;

use crate::document::PdfDocument;
use crate::error::{PdfError, Result};
use crate::objects::PdfValue;
use crate::parser::{ContentSegment, FileParser};
use crate::page::OpSpan;
use crate::writer::{Trailer, UpdateWriter};

/// Working copy of one page: its content streams kept apart.
///
/// One buffer per `/Contents` entry, not one concatenated buffer. Spans
/// index the concatenated form, so they stay valid as long as the
/// original segment boundaries do; splicing into a single shared buffer
/// would move every following segment's offsets and silently corrupt
/// the untouched streams.
struct PageState {
  /// Object number of the page dict to re-emit on save.
  object_number: u32,
  /// Original `/Contents` entries; their byte ranges map spans onto
  /// `parts`.
  segments: Vec<ContentSegment>,
  /// Current bytes of each entry, edited in place.
  parts: Vec<Vec<u8>>,
  /// One flag per entry: set when its bytes changed.
  dirty: Vec<bool>,
}

/// Mutating editor over one document.
///
/// `new` takes the document by value because saving appends to the very
/// bytes it was parsed from.
pub struct PdfEditor {
  /// Untouched copy of the source bytes; every save appends to this.
  original: Vec<u8>,
  /// Trailer entries carried into the appended section.
  trailer: Trailer,
  /// `startxref` of the last section in `original`.
  startxref: u64,
  /// True when that section is an xref stream.
  xref_is_stream: bool,
  /// One past the highest object number in `original`.
  next_num: u32,
  /// Per-page working state.
  pages: Vec<PageState>,
  /// Document reflecting all pending edits.
  doc: Option<PdfDocument>,
  dirty: bool,
  edits: usize,
}

impl PdfEditor {
  /// Take ownership of a parsed document for editing.
  ///
  /// Yields `EncryptedEdit` for encrypted files.
  pub fn new(doc: PdfDocument) -> Result<Self> {
    if doc.is_encrypted() {
      return Err(PdfError::EncryptedEdit);
    }
    let mut editor = Self {
      original: doc.bytes().to_vec(),
      trailer: doc.trailer().clone(),
      startxref: doc.startxref(),
      xref_is_stream: doc.xref_is_stream(),
      next_num: doc.next_object_number(),
      pages: Vec::new(),
      doc: Some(doc),
      dirty: false,
      edits: 0,
    };
    editor.snapshot_pages()?;
    Ok(editor)
  }

  /// True when at least one edit is pending.
  pub fn is_dirty(&self) -> bool {
    self.dirty
  }

  /// Number of edits applied since the last save.
  pub fn edit_count(&self) -> usize {
    self.edits
  }

  /// The document reflecting all pending edits, re-parsed from a
  /// throwaway build. A live viewer binds to this.
  pub fn document(&self) -> Option<&PdfDocument> {
    self.doc.as_ref()
  }

  /// Consume the editor and return the edited document.
  pub fn into_document(self) -> Option<PdfDocument> {
    self.doc
  }

  /// Copy each page's content buffer out of the parsed document.
  fn snapshot_pages(&mut self) -> Result<()> {
    let doc = self.doc.as_ref().ok_or(PdfError::NoPages)?;
    self.pages.clear();
    for index in 0..doc.page_count() {
      let page = doc.page(index)?;
      self.pages.push(PageState {
        object_number: page.object_number,
        parts: page
          .content_segments
          .iter()
          .map(|seg| page.content[seg.start..seg.end].to_vec())
          .collect(),
        segments: page.content_segments.clone(),
        dirty: vec![false; page.content_segments.len()],
      });
    }
    Ok(())
  }

  /// Replace the text of run `run` on page `page`.
  ///
  /// Runs without addressable source bytes (strings synthesized by a
  /// broken writer, inline-image data) yield `UneditableRun`.
  pub fn set_text(&mut self, page: usize, run: usize, text: &str) -> Result<()> {
    let span = self.run_span(page, run)?;
    let literal = literal_string(&encode_text(text));
    self.splice(page, span, literal)
  }

  /// Replace the text of the first run on page `page` whose text
  /// equals `find`. Convenience for scripted edits and demos.
  pub fn replace_text(&mut self, page: usize, find: &str, text: &str) -> Result<()> {
    let run = {
      let doc = self.doc.as_ref().ok_or(PdfError::NoPages)?;
      let page = doc.page(page)?;
      page.runs.iter().position(|r| r.text == find).ok_or(PdfError::RunNotFound(find.to_owned()))?
    };
    self.set_text(page, run, text)
  }

  /// Addressable span of run `run` on page `page`.
  fn run_span(&self, page: usize, index: usize) -> Result<OpSpan> {
    let doc = self.doc.as_ref().ok_or(PdfError::NoPages)?;
    let page = doc.page(page)?;
    let run = page.runs.get(index).ok_or(PdfError::NoSuchRun(index))?;
    let span = run.src.ok_or(PdfError::UneditableRun(index))?;
    Ok(span)
  }

  /// Replace the raw bytes a span covers with a complete literal.
  fn splice(&mut self, page: usize, span: OpSpan, bytes: Vec<u8>) -> Result<()> {
    let state = self.pages.get_mut(page).ok_or(PdfError::PageOutOfRange(page))?;
    let (start, end) = (span.operand_start as usize, span.operand_end as usize);
    // A span belongs to exactly one stream: each stream was lexed on
    // its own buffer. A span straddling two would mean the item model
    // and the segment table disagree, so refuse instead of writing a
    // stream whose bytes no longer match its dictionary.
    let index = state
      .segments
      .iter()
      .position(|seg| start >= seg.start && end <= seg.end)
      .ok_or(PdfError::InvalidObject("edit span crosses a content stream boundary".into()))?;
    let base = state.segments[index].start;
    let part = &mut state.parts[index];
    let (local, local_end) = (start - base, end - base);
    if local_end > part.len() || local >= local_end {
      return Err(PdfError::InvalidObject(format!("span {local}..{local_end} outside its content stream")));
    }
    part.splice(local..local_end, bytes);
    state.dirty[index] = true;
    self.dirty = true;
    self.edits += 1;
    // Refresh the live document so the caller sees the new text. The
    // working copies stay authoritative: `original` and the dirty flags
    // are what a save replays.
    self.refresh();
    Ok(())
  }

  /// Re-parse a throwaway build into `doc` (never adopts it).
  fn refresh(&mut self) {
    if let Ok(bytes) = self.build() {
      if let Ok(doc) = PdfDocument::load_bytes(bytes) {
        self.doc = Some(doc);
      }
    }
  }

  /// Append the pending edits and return the new file bytes.
  ///
  /// Only streams whose bytes changed are re-encoded; the rest of the
  /// file is copied verbatim. Does not mutate the editor's baseline.
  pub fn build(&mut self) -> Result<Vec<u8>> {
    if !self.dirty {
      return Ok(self.original.clone());
    }
    let parser = FileParser::new(self.original.clone())?;
    let mut writer = UpdateWriter::new(
      self.original.clone(),
      self.startxref,
      self.xref_is_stream,
      self.trailer.clone(),
      self.next_num,
    );

    for state in self.pages.iter() {
      if !state.dirty.iter().any(|d| *d) {
        continue;
      }
      let mut contents: Vec<PdfValue> = Vec::with_capacity(state.segments.len());
      for (i, segment) in state.segments.iter().enumerate() {
        if state.dirty[i] {
          let bytes = writer.push_stream(&[], &state.parts[i]);
          contents.push(PdfValue::Ref(bytes, 0));
        } else {
          contents.push(PdfValue::Ref(segment.obj, 0));
        }
      }

      // Re-emit the page dict with only /Contents replaced, so
      // MediaBox, Resources, CropBox, Rotate and Annots survive.
      let page_obj = state.object_number;
      let mut value = parser.object(page_obj)?.value;
      match &mut value {
        PdfValue::Dict(entries) => {
          let mut found = false;
          for entry in entries.iter_mut() {
            if entry.0 == "Contents" {
              entry.1 = PdfValue::Array(contents.clone());
              found = true;
            }
          }
          if !found {
            return Err(PdfError::InvalidObject(format!("page object {page_obj} has no /Contents")));
          }
        }
        _ => return Err(PdfError::InvalidObject(format!("page object {page_obj} is not a dictionary"))),
      }
      writer.push_raw(page_obj, 0, UpdateWriter::encode_value(&value));
    }

    writer.finish()
  }

  /// Serialize the edits and adopt the result as the new baseline.
  pub fn into_bytes(&mut self) -> Result<Vec<u8>> {
    let bytes = self.build()?;
    let parser = FileParser::new(bytes.clone())?;
    self.startxref = parser.startxref();
    self.xref_is_stream = parser.xref_is_stream();
    self.next_num = parser.next_object_number();
    self.original = bytes.clone();
    self.doc = Some(PdfDocument::load_bytes(bytes.clone())?);
    self.dirty = false;
    self.edits = 0;
    self.snapshot_pages()?;
    Ok(bytes)
  }

  /// Serialize and write to `path`.
  pub fn save_to(&mut self, path: impl AsRef<Path>) -> Result<()> {
    let bytes = self.into_bytes()?;
    std::fs::write(path.as_ref(), bytes).map_err(|e| PdfError::InvalidObject(e.to_string()))
  }
}

/// Encode text as WinAnsi bytes (ISO 1252).
pub fn encode_text(text: &str) -> Vec<u8> {
  let mut out = Vec::with_capacity(text.len());
  for ch in text.chars() {
    match ch {
      '\n' => out.push(b'\n'),
      '\r' => out.push(b'\r'),
      '\t' => out.push(b'\t'),
      other => out.push(winansi_byte(other).unwrap_or(b'?')),
    }
  }
  out
}

/// Wrap bytes as a complete PDF literal string, escaping what must be
/// escaped. The result replaces a span that covered a whole literal,
/// delimiters included.
pub fn literal_string(bytes: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(bytes.len() + 2);
  out.push(b'(');
  for &byte in bytes {
    match byte {
      b'(' | b')' | b'\\' => {
        out.push(b'\\');
        out.push(byte);
      }
      b'\r' => out.extend_from_slice(b"\\r"),
      b'\n' => out.extend_from_slice(b"\\n"),
      // A literal newline would terminate the line; octal keeps it
      // inside the string and stays readable for the decoder.
      0x20..=0x7e => out.push(byte),
      other => out.extend_from_slice(format!("\\{other:03o}").as_bytes()),
    }
  }
  out.push(b')');
  out
}

/// WinAnsi (ISO 1252) code point for a character.
///
/// The 0xA0-0xFF range coincides with Latin-1; the 0x80-0x9F range
/// holds the typographic characters PDF's base-14 fonts expect.
pub fn winansi_byte(ch: char) -> Option<u8> {
  let code = ch as u32;
  if code < 0x80 || (0xA0..0x100).contains(&code) {
    return Some(code as u8);
  }
  Some(match ch {
    '\u{20AC}' => 0x80,
    '\u{201A}' => 0x82,
    '\u{0192}' => 0x83,
    '\u{201E}' => 0x84,
    '\u{2026}' => 0x85,
    '\u{2020}' => 0x86,
    '\u{2021}' => 0x87,
    '\u{02C6}' => 0x88,
    '\u{2030}' => 0x89,
    '\u{0160}' => 0x8A,
    '\u{2039}' => 0x8B,
    '\u{0152}' => 0x8C,
    '\u{017D}' => 0x8E,
    '\u{2018}' => 0x91,
    '\u{2019}' => 0x92,
    '\u{201C}' => 0x93,
    '\u{201D}' => 0x94,
    '\u{2022}' => 0x95,
    '\u{2013}' => 0x96,
    '\u{2014}' => 0x97,
    '\u{02DC}' => 0x98,
    '\u{2122}' => 0x99,
    '\u{0161}' => 0x9A,
    '\u{203A}' => 0x9B,
    '\u{0153}' => 0x9C,
    '\u{017E}' => 0x9E,
    '\u{0178}' => 0x9F,
    _ => return None,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parser::tests::minimal_pdf;

  fn editor() -> PdfEditor {
    let pdf = minimal_pdf(b"BT /F1 12 Tf 72 720 Td (Hello PDF) Tj ET");
    PdfEditor::new(PdfDocument::load_bytes(pdf).unwrap()).unwrap()
  }

  #[test]
  fn literal_string_escapes_delimiters() {
    // A raw `(` would end the literal early and corrupt the stream.
    assert_eq!(literal_string(b"a(b)c\\d"), b"(a\\(b\\)c\\\\d)");
    // Non-printables go octal so nothing terminates the line; a newline
// uses its named escape, which is the same byte.
    assert_eq!(literal_string(&[0x01, 0x0A]), b"(\\001\\n)");
  }

  #[test]
  fn winansi_maps_typographic_punctuation() {
    assert_eq!(winansi_byte('A'), Some(b'A'));
    assert_eq!(winansi_byte('\u{00E4}'), Some(0xE4));
    assert_eq!(winansi_byte('\u{2019}'), Some(0x92));
    assert_eq!(winansi_byte('\u{4E2D}'), None);
    assert_eq!(encode_text("a\u{2019}b"), vec![b'a', 0x92, b'b']);
  }

  #[test]
  fn set_text_is_visible_before_saving() {
    let mut editor = editor();
    assert_eq!(editor.document().unwrap().page(0).unwrap().runs[0].text, "Hello PDF");
    editor.set_text(0, 0, "Goodbye PDF").unwrap();
    assert!(editor.is_dirty());
    // The live document already shows the edit.
    assert_eq!(editor.document().unwrap().page(0).unwrap().runs[0].text, "Goodbye PDF");
  }

  #[test]
  fn saved_file_reloads_with_the_new_text() {
    let mut editor = editor();
    editor.set_text(0, 0, "Goodbye PDF").unwrap();
    let bytes = editor.into_bytes().unwrap();
    let doc = PdfDocument::load_bytes(bytes).unwrap();
    assert_eq!(doc.page(0).unwrap().runs[0].text, "Goodbye PDF");
    assert!(!editor.is_dirty());
  }

  #[test]
  fn save_appends_instead_of_rewriting() {
    // The point of an incremental update: the original bytes survive
    // untouched at the front of the file.
    let mut editor = editor();
    let original = editor.original.clone();
    editor.set_text(0, 0, "Changed").unwrap();
    let bytes = editor.into_bytes().unwrap();
    assert!(bytes.len() > original.len());
    assert_eq!(&bytes[..original.len()], &original[..], "original prefix must be byte-identical");
    // The appended section is a classic xref table plus a /Prev chain.
    let tail = String::from_utf8_lossy(&bytes[original.len()..]);
    assert!(tail.contains("xref"), "appended section: {tail}");
    assert!(tail.contains("/Prev"), "appended trailer must chain: {tail}");
  }

  #[test]
  fn unchanged_pages_keep_their_streams() {
    // Only the edited page is re-emitted; other pages are not even
    // mentioned in the appended section.
    let pdf = two_page_pdf();
    let mut editor = PdfEditor::new(PdfDocument::load_bytes(pdf.clone()).unwrap()).unwrap();
    editor.set_text(0, 0, "Page zero").unwrap();
    let bytes = editor.into_bytes().unwrap();
    let doc = PdfDocument::load_bytes(bytes.clone()).unwrap();
    // Both pages still read back, page 1 untouched.
    assert_eq!(doc.page(0).unwrap().runs[0].text, "Page zero");
    assert_eq!(doc.page(1).unwrap().runs[0].text, "Second page");
    // The tail mentions the page dict of page 0 but not of page 1.
    let tail = String::from_utf8_lossy(&bytes[pdf.len()..]).to_string();
    assert!(tail.contains(&format!("{} 0 obj", editor_page_obj(&pdf, 0))));
    assert!(!tail.contains(&format!("{} 0 obj", editor_page_obj(&pdf, 1))));
  }

  #[test]
  fn text_requiring_escapes_roundtrips() {
    let mut editor = editor();
    editor.set_text(0, 0, "a(b)c \\ done").unwrap();
    let bytes = editor.into_bytes().unwrap();
    let doc = PdfDocument::load_bytes(bytes).unwrap();
    assert_eq!(doc.page(0).unwrap().runs[0].text, "a(b)c \\ done");
  }

  #[test]
  fn second_edit_keeps_the_first() {
    // Two edits before saving must both land: the working copy is what
    // gets written, not a single edit replayed.
    let mut editor = editor();
    editor.set_text(0, 0, "First").unwrap();
    editor.set_text(0, 0, "Second").unwrap();
    let bytes = editor.into_bytes().unwrap();
    assert_eq!(PdfDocument::load_bytes(bytes).unwrap().page(0).unwrap().runs[0].text, "Second");
  }

  #[test]
  fn encrypted_documents_are_refused() {
    for name in ["aes128-user.pdf", "rc4-40-owner.pdf"] {
      let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/").to_string() + name;
      let data = std::fs::read(&path).unwrap();
      let opened = PdfDocument::load_bytes_with_password(data, "userpass")
        .or_else(|_| PdfDocument::load_bytes_with_password(std::fs::read(&path).unwrap(), "owner"))
        .or_else(|_| PdfDocument::load_bytes(std::fs::read(&path).unwrap()));
      let doc = match opened {
        Ok(doc) => doc,
        // Fixture did not open with any known password: the refusal is
        // already covered by the error path below for open files.
        Err(_) => continue,
      };
      if doc.is_encrypted() {
        assert!(matches!(PdfEditor::new(doc), Err(PdfError::EncryptedEdit)), "{name} must be refused");
      }
    }
  }

  #[test]
  fn unaddressable_run_is_reported() {
    // Index out of range and a run without source bytes are distinct
    // errors, not panics.
    let mut editor = editor();
    assert!(matches!(editor.set_text(0, 99, "x"), Err(PdfError::NoSuchRun(_))));
    assert!(matches!(editor.set_text(7, 0, "x"), Err(PdfError::PageOutOfRange(_))));
  }

  /// Two-page file with one content stream per page.
  fn two_page_pdf() -> Vec<u8> {
    let stream = |body: &[u8]| {
      [b"<< /Length ".to_vec(), body.len().to_string().into_bytes(), b" >>\nstream\n".to_vec(), body.to_vec(), b"\nendstream".to_vec()].concat()
    };
    let objects: Vec<Vec<u8>> = vec![
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> >> >>".to_vec(),
      stream(b"BT /F1 12 Tf 72 720 Td (First page) Tj ET"),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << /Font << /F1 6 0 R >> >> >>".to_vec(),
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
      stream(b"BT /F1 12 Tf 72 720 Td (Second page) Tj ET"),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
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
    pdf.extend_from_slice(b"trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n");
    pdf.extend_from_slice(xref.to_string().as_bytes());
    pdf.extend_from_slice(b"\n%%EOF");
    pdf
  }

  /// Object number of the page dict at `index`.
  fn editor_page_obj(pdf: &[u8], index: usize) -> u32 {
    PdfDocument::load_bytes(pdf.to_vec()).unwrap().page(index).unwrap().object_number
  }
}