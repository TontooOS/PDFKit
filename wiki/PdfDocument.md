# PdfDocument

`PdfDocument` loads a PDF file into interpreted pages plus outlines and
metadata. It owns the parsed pages so the viewer and the editor work on
real PDF coordinates instead of a rasterized bitmap.

## Types

```rust
pub struct PdfDocument { /* pages, outlines, info */ }
```

```rust
pub struct PdfPage {
  pub number: usize,
  pub width: f32,
  pub height: f32,
  pub origin_x: f32,
  pub origin_y: f32,
  pub runs: Vec<PdfTextRun>,
  pub items: Vec<PageItem>,
  pub annotations: Vec<Annotation>,
}
```

See [Graphics.md](Graphics.md) for `PageItem`, [Fonts.md](Fonts.md) for
`PdfTextRun` and [Annotations.md](Annotations.md) for `Annotation`.

## Constructors

### `load_bytes`

```rust
pub fn load_bytes(data: Vec<u8>) -> Result<Self>
```

Parses a document from memory with the empty password. Encrypted files
yield `Err(NeedsPassword)`; corrupt input yields the matching variant.

### `load_bytes_with_password`

```rust
pub fn load_bytes_with_password(data: Vec<u8>, password: &str) -> Result<Self>
```

Parses with an explicit password. Yields `Err(WrongPassword)` when the
password does not open the file. Supports V1/V2 (RC4), V4 (AESV2) and
V5 (AES-256, R5/R6); see [DocumentStructure.md](DocumentStructure.md).

### `load_file`

```rust
pub fn load_file(path: &str) -> Result<Self>
```

Reads the file at `path` and parses it with the empty password.

### `load_file_with_password`

```rust
pub fn load_file_with_password(path: &str, password: &str) -> Result<Self>
```

Reads the file at `path` and parses it with a password.

## Functions

| Function | Signature | Behavior |
|---|---|---|
| `page_count` | `page_count(&self) -> usize` | Number of pages in document order |
| `page` | `page(&self, index: usize) -> Result<&PdfPage>` | Page by zero-based index; `Err(PageOutOfRange)` when invalid |
| `is_empty_text` | `is_empty_text(&self) -> bool` | True when no page carries text runs |
| `outlines` | `outlines(&self) -> &[Outline]` | Bookmarks with resolved page targets |
| `info` | `info(&self) -> &DocInfo` | Metadata from the trailer `/Info` dict |
| `text` | `text(&self) -> String` (`PdfPage`) | Plain text, runs ordered top-to-bottom |

## Errors

| Variant | Meaning |
|---|---|
| `Empty` | Input was empty |
| `InvalidHeader` | Missing `%PDF-` header |
| `XrefNotFound` | No `startxref` offset and no scannable objects |
| `ObjectNotFound(n)` | Indirect object `n` does not exist |
| `InvalidObject(msg)` | Malformed object structure |
| `UnsupportedFilter(name)` | Stream `/Filter` is CCITT or JBIG2 |
| `StreamDecode(msg)` | Filter or predictor decoding failed |
| `NoPages` | Document contains no pages |
| `PageOutOfRange(i)` | Page index `i` is invalid |
| `ContentParse(msg)` | Content stream tokenizing failed |
| `NeedsPassword` | Encrypted file, empty password rejected |
| `WrongPassword` | Explicit password rejected |
| `UnsupportedCrypt(msg)` | Unknown `V`/`R` or handler |
| `Lang(msg)` | Language file is not valid JSON |

## Usage / Example

```rust
use pdfkit::PdfDocument;

let doc = PdfDocument::load_file("/path/to/file.pdf").unwrap();
assert!(doc.page_count() >= 1);
for outline in doc.outlines() {
  println!("{}", outline.title);
}
let first = doc.page(0).unwrap();
println!("{}", first.text());
```

## Cross References

- [PdfView.md](PdfView.md) – renders these pages as a TontooUI view
- [DocumentStructure.md](DocumentStructure.md) – file layout and encryption
- [Annotations.md](Annotations.md) – outlines, links and metadata
- [Language.md](Language.md) – user-facing error strings
