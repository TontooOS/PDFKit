# PDFKit – Wiki

PDFKit is the native PDF document model and viewer for TontooOS. It parses
real PDF files into positioned text runs and renders them as a TontooUI
`View` through the shared `FontSystem` (SF Pro, Parley layout, Vello glyphs).

- Repository: https://github.com/TontooOS/PDFKit
- License: TCL
- Version: 26.1.0

## Feature Index

| Feature | File | Description |
|---|---|---|
| Main index | [MAIN.md](MAIN.md) | This page |
| Rules | [RULE.md](RULE.md) | Development and usage rules |
| PdfDocument | [PdfDocument.md](PdfDocument.md) | File parsing, page tree and text runs |
| PdfView | [PdfView.md](PdfView.md) | TontooUI view with page, zoom and theme |
| Language | [Language.md](Language.md) | `lang/en_us.json` and `lang/de_de.json` strings |

## Quick Start

```rust
use pdfkit::{PdfDocument, PdfView};

let doc = PdfDocument::load_file("/path/to/file.pdf").unwrap();
let view = PdfView::new(doc);
```

See [PdfDocument.md](PdfDocument.md) for details.

## Changelog

- 2026-09-26: Initial wiki, text-only document model (`PdfDocument`,
  `PdfPage`, `PdfTextRun`) and `PdfView` (page, zoom, theme, Vello text).
