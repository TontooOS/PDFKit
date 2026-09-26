//! PDFKit: native PDF document model and TontooUI viewer for TontooOS.
//!
//! The crate parses real PDF files (header, xref table, page tree,
//! FlateDecode content streams) into a page model of positioned text
//! runs and renders them through the shared TontooUI `FontSystem`
//! (SF Pro, Parley layout, Vello glyphs). Nothing is rasterized to a
//! bitmap, so the editor milestone can select and edit text in place.
//!
//! v0.1 renders text only: `BT`/`ET`, `Tf`, `Tc`, `Tw`, `TL`, `Tm`,
//! `Td`, `TD`, `T*`, `Tj`, `TJ`, `'`, `"`. Graphics, images, colors
//! and annotations follow in later milestones.
//!
//! ```rust,no_run
//! use pdfkit::{PdfDocument, PdfView};
//!
//! let bytes = std::fs::read("/path/to/file.pdf").unwrap();
//! let doc = PdfDocument::load_bytes(bytes).unwrap();
//! let pages = doc.page_count();
//! let view = PdfView::new(doc);
//! assert_eq!(view.page_count(), pages);
//! ```

pub mod document;
pub mod error;
pub mod lang;
pub mod objects;
pub mod page;
pub mod parser;
pub mod view;

pub use document::PdfDocument;
pub use error::{PdfError, Result};
pub use page::{PdfPage, PdfTextRun, decode_text};
pub use parser::{FileParser, PageFont, ParsedPage};
pub use view::{PdfView, PDF_BG_DARK, PDF_BG_LIGHT, PDF_TEXT_DARK, PDF_TEXT_LIGHT};
