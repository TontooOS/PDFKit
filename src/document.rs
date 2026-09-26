use crate::error::{PdfError, Result};
use crate::page::PdfPage;
use crate::parser::FileParser;

/// A loaded PDF document: parsed structure plus interpreted pages.
///
/// The document owns its bytes so pages can be re-interpreted on
/// demand (needed later for the editor without re-reading the file).
/// v0.1 parses eagerly at load; large documents stream later.
#[derive(Debug)]
pub struct PdfDocument {
  pages: Vec<PdfPage>,
}

impl PdfDocument {
  /// Parse a document from memory.
  pub fn load_bytes(data: Vec<u8>) -> Result<Self> {
    let parser = FileParser::new(data)?;
    Self::from_parser(&parser)
  }

  /// Parse a document from a file path.
  pub fn load_file(path: &str) -> Result<Self> {
    let data = std::fs::read(path).map_err(|e| PdfError::InvalidObject(e.to_string()))?;
    Self::load_bytes(data)
  }

  fn from_parser(parser: &FileParser) -> Result<Self> {
    let parsed = parser.pages()?;
    let mut pages = Vec::with_capacity(parsed.len());
    for item in &parsed {
      pages.push(PdfPage::interpret(item.index, item.media_box, &item.content, &item.fonts)?);
    }
    if pages.is_empty() {
      return Err(PdfError::NoPages);
    }
    Ok(Self { pages })
  }

  /// Number of pages in the document.
  pub fn page_count(&self) -> usize {
    self.pages.len()
  }

  /// Access page `index` (zero-based). Returns `PageOutOfRange` when
  /// the index is invalid.
  pub fn page(&self, index: usize) -> Result<&PdfPage> {
    self.pages.get(index).ok_or(PdfError::PageOutOfRange(index))
  }

  /// True when the document has no text on any page.
  pub fn is_empty_text(&self) -> bool {
    self.pages.iter().all(|p| p.runs.is_empty())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parser::tests::minimal_pdf;

  #[test]
  fn loads_single_page_document() {
    let pdf = minimal_pdf(b"BT /F1 12 Tf 72 720 Td (Hello PDF) Tj ET");
    let doc = PdfDocument::load_bytes(pdf).unwrap();
    assert_eq!(doc.page_count(), 1);
    assert!(doc.page(0).unwrap().text().contains("Hello PDF"));
  }

  #[test]
  fn rejects_bad_page_index() {
    let pdf = minimal_pdf(b"BT /F1 12 Tf (x) Tj ET");
    let doc = PdfDocument::load_bytes(pdf).unwrap();
    assert_eq!(doc.page(7).unwrap_err(), PdfError::PageOutOfRange(7));
  }

  #[test]
  fn rejects_non_pdf() {
    assert_eq!(PdfDocument::load_bytes(b"hello".to_vec()).unwrap_err(), PdfError::InvalidHeader);
  }
}
