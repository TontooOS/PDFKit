# PdfDocument

`PdfDocument` loads a PDF file into a page model of positioned text runs.
It owns the parsed pages so the viewer and the later editor can work on
real PDF coordinates instead of a rasterized bitmap.

Supported in v0.1: `%PDF-` header, classic `xref` tables (with an object
scan fallback), catalog and page tree walk with `/MediaBox` inheritance,
`/FlateDecode` content streams and the text operators `BT`, `ET`, `Tf`,
`Tc`, `Tw`, `TL`, `Tm`, `Td`, `TD`, `T*`, `Tj`, `TJ`, `'` and `"`.

## Types

```rust
pub struct PdfDocument { /* pages */ }
```

```rust
pub struct PdfPage {
  pub number: usize,
  pub width: f32,
  pub height: f32,
  pub runs: Vec<PdfTextRun>,
}
```

```rust
pub struct PdfTextRun {
  pub text: String,
  pub x: f32,
  pub y: f32,
  pub font_size: f32,
  pub bold: bool,
  pub font_name: String,
}
```

Coordinates are PDF points with the origin at the bottom-left of the page.
`bold` is derived from the `/BaseFont` name containing `Bold`. Other
operators (graphics, color, images, annotations) are skipped with their
operands and follow in later milestones.

## Constructors

### `load_bytes`

```rust
pub fn load_bytes(data: Vec<u8>) -> Result<Self>
```

Parses a document from memory. Returns `Err` when the input is empty,
the header is missing, the xref table cannot be found, or the file has
no pages.

### `load_file`

```rust
pub fn load_file(path: &str) -> Result<Self>
```

Reads the file at `path` and parses it. Returns `Err` when the file
cannot be read or parsing fails (see `load_bytes`).

## Functions

| Function | Signature | Behavior |
|---|---|---|
| `page_count` | `page_count(&self) -> usize` | Number of pages in document order |
| `page` | `page(&self, index: usize) -> Result<&PdfPage>` | Page by zero-based index; `Err(PageOutOfRange)` when invalid |
| `is_empty_text` | `is_empty_text(&self) -> bool` | True when no page carries text runs |
| `text` | `text(&self) -> String` (`PdfPage`) | Plain text, runs ordered top-to-bottom, left-to-right |

## Errors

| Variant | Meaning |
|---|---|
| `Empty` | Input was empty |
| `InvalidHeader` | Missing `%PDF-` header |
| `XrefNotFound` | No `startxref` offset and no scannable objects |
| `ObjectNotFound(n)` | Indirect object `n` does not exist |
| `InvalidObject(msg)` | Malformed object structure |
| `UnsupportedFilter(name)` | Stream `/Filter` other than `FlateDecode` |
| `StreamDecode(msg)` | Flate decompression failed |
| `NoPages` | Document contains no pages |
| `PageOutOfRange(i)` | Page index `i` is invalid |
| `ContentParse(msg)` | Content stream tokenizing failed |

## Usage / Example

```rust
use pdfkit::PdfDocument;

let doc = PdfDocument::load_file("/path/to/file.pdf").unwrap();
assert!(doc.page_count() >= 1);
let first = doc.page(0).unwrap();
for run in &first.runs {
  println!("{}pt @ ({}, {}): {}", run.font_size, run.x, run.y, run.text);
}
```

## Cross References

- [PdfView.md](PdfView.md) – renders these pages as a TontooUI view
- [Language.md](Language.md) – user-facing error and viewer strings
