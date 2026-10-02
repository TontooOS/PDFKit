use std::fmt;

/// Errors returned by PDFKit parsing and page access.
///
/// All variants carry English messages so they can be shown through
/// the `lang/` files (`pdfkit.error.*` keys) without translation drift.
#[derive(Debug, Clone, PartialEq)]
pub enum PdfError {
  /// Input was empty.
  Empty,
  /// Missing `%PDF-` header in the first bytes.
  InvalidHeader,
  /// No `startxref` offset found at the end of the file.
  XrefNotFound,
  /// Indirect object `N` does not exist in the file.
  ObjectNotFound(u32),
  /// Object structure is malformed; carries a detail message.
  InvalidObject(String),
  /// A content stream advertises an unsupported `/Filter`.
  UnsupportedFilter(String),
  /// A stream failed to decode; carries a detail message.
  StreamDecode(String),
  /// The document contains no pages.
  NoPages,
  /// Requested page index is out of range.
  PageOutOfRange(usize),
  /// Content stream tokenizing failed; carries a detail message.
  ContentParse(String),
  /// A `lang/` lookup file failed to parse; carries a detail message.
  Lang(String),
  /// The file is encrypted; open it with a password.
  NeedsPassword,
  /// The password does not open the file.
  WrongPassword,
  /// Unsupported crypt filter or handler; carries a detail message.
  UnsupportedCrypt(String),
  /// The file is encrypted; editing would have to re-encrypt appended
  /// bytes, which is not implemented.
  EncryptedEdit,
  /// No text run with that index on the requested page.
  NoSuchRun(usize),
  /// The run exists but its source bytes are not addressable, so it
  /// cannot be rewritten (inline-image data, synthesized strings).
  UneditableRun(usize),
  /// Two edits overlap in the same content stream.
  OverlappingEdit,
  /// No run on the page carries the searched-for text.
  RunNotFound(String),
}

impl fmt::Display for PdfError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Empty => write!(f, "empty PDF input"),
      Self::InvalidHeader => write!(f, "missing %PDF- header"),
      Self::XrefNotFound => write!(f, "startxref offset not found"),
      Self::ObjectNotFound(n) => write!(f, "object {n} not found"),
      Self::InvalidObject(detail) => write!(f, "invalid object: {detail}"),
      Self::UnsupportedFilter(name) => write!(f, "unsupported stream filter: {name}"),
      Self::StreamDecode(detail) => write!(f, "stream decode failed: {detail}"),
      Self::NoPages => write!(f, "document contains no pages"),
      Self::PageOutOfRange(i) => write!(f, "page index out of range: {i}"),
      Self::ContentParse(detail) => write!(f, "content parse failed: {detail}"),
      Self::Lang(detail) => write!(f, "language file error: {detail}"),
      Self::NeedsPassword => write!(f, "file is encrypted, password required"),
      Self::WrongPassword => write!(f, "wrong password"),
      Self::UnsupportedCrypt(detail) => write!(f, "unsupported encryption: {detail}"),
      Self::EncryptedEdit => write!(f, "editing encrypted files is not supported"),
      Self::NoSuchRun(i) => write!(f, "no text run with index {i}"),
      Self::UneditableRun(i) => write!(f, "text run {i} has no addressable source bytes"),
      Self::OverlappingEdit => write!(f, "edits overlap in the same content stream"),
      Self::RunNotFound(text) => write!(f, "no run with text {text:?}"),
    }
  }
}

impl std::error::Error for PdfError {}

/// Convenience alias for PDFKit results.
pub type Result<T> = std::result::Result<T, PdfError>;
