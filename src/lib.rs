//! PDFKit: native PDF document model and TontooUI viewer for TontooOS.
//!
//! The crate parses real PDF files (header, xref tables and streams,
//! page tree, all standard stream filters) into a vector/text page
//! model and renders it through the shared TontooUI `FontSystem`
//! (SF Pro, Parley layout, Vello glyphs and paths). Nothing is
//! rasterized to a bitmap, so the editor milestone can select and
//! edit text in place.
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

pub mod annot;
pub mod crypt;
pub mod document;
pub mod color;
pub mod error;
pub mod filter;
pub mod font;
pub mod graphics;
pub mod image;
pub mod lang;
pub mod objects;
pub mod page;
pub mod parser;
pub mod view;

pub use color::{Function, ResolvedPattern, ResolvedShading, calgray_to_rgb, calrgb_to_rgb, indexed_lookup, lab_to_rgb, parse_function, parse_sampled, shading_stops};
pub use annot::{Annotation, DocInfo, LinkTarget, Outline, parse_annotation, parse_info, parse_outlines, pdfdoc_to_string};
pub use crypt::{Cfm, CryptState, PADDING, aes128_cbc_decrypt, aes128_cbc_encrypt, aes256_cbc_decrypt, md5sum, pdf20_hash, rc4, sha256sum};
pub use document::PdfDocument;
pub use font::{CMap, DecoderKind, FontDecoder, RangeDst, apply_differences, glyph_name_to_char, parse_cmap};
pub use error::{PdfError, Result};
pub use graphics::{
  ExtGState, FillRule, FontInfo, GradientItem, InlineVal, MapResources, Marked, Matrix, MAX_FORM_DEPTH, NoResources,
  PageItem, PathItem, PathSeg, PlacedImage, ResourceProvider, Rgb, StrokeStyle, XObjectResult, cmyk_to_rgb, interpret,
  interpret_with, text_runs,
};
pub use image::{DecodedImage, apply_alpha, apply_constant_alpha, decode_jpeg, decode_mask_alpha, decode_samples, decode_smask_alpha};
pub use page::{PdfPage, PdfTextRun, decode_text};
pub use parser::{FileParser, PageFont, ParsedPage};
pub use view::{PdfView, PDF_BG_DARK, PDF_BG_LIGHT, PDF_PAPER, PDF_TEXT_DARK, PDF_TEXT_LIGHT};
