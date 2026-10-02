# PDFKit – Wiki

PDFKit is the native PDF document model and viewer for TontooOS. It parses
real PDF files (structure, filters, encryption, graphics, fonts, images,
annotations) into a vector/text page model and renders it as a TontooUI
`View` through the shared `FontSystem` (SF Pro, Parley layout, Vello).

- Repository: https://github.com/TontooOS/PDFKit
- License: TCL
- Version: 27.0.0

## Feature Index

| Feature | File | Description |
|---|---|---|
| Main index | [MAIN.md](MAIN.md) | This page |
| Rules | [RULE.md](RULE.md) | Development and usage rules |
| PdfDocument | [PdfDocument.md](PdfDocument.md) | Loading, pages, encryption, outlines, metadata |
| PdfView | [PdfView.md](PdfView.md) | TontooUI view with page, zoom and annotations |
| DocumentStructure | [DocumentStructure.md](DocumentStructure.md) | Header, xref, object streams, filters |
| Graphics | [Graphics.md](Graphics.md) | Paths, colors, transparency, patterns, shadings |
| Fonts | [Fonts.md](Fonts.md) | Encodings, ToUnicode CMaps, CID fonts |
| Images | [Images.md](Images.md) | Image and form XObjects, inline images, masks |
| Annotations | [Annotations.md](Annotations.md) | Annots, links, outlines, marked content |
| Language | [Language.md](Language.md) | `lang/en_us.json` and `lang/de_de.json` strings |

## Quick Start

```rust
use pdfkit::{PdfDocument, PdfView};

let doc = PdfDocument::load_file("/path/to/file.pdf").unwrap();
let view = PdfView::new(doc);
```

Encrypted files open with a password:

```rust
use pdfkit::PdfDocument;

let doc = PdfDocument::load_file_with_password("/path/to/file.pdf", "userpass").unwrap();
```

See [PdfDocument.md](PdfDocument.md) for details.

## Changelog

- 2026-10-02: Text run positions moved out of the layout cache into
  draw-time mapping: run origins follow `place()` without re-typesetting,
  so stacked/scrolled viewers no longer pile every page's text on one
  spot while paths and images stay in place.
- 2026-09-28: Visual compare loop fully green (105/105): rotated
  text and page `/Rotate`, spec-correct LZW codes, calibrated CMYK
  table, CropBox frames, stencil polarity, annotation verdicts.
- 2026-09-26: Full standard pass M1-M8: filters, xref streams, graphics,
  transparency, fonts, images, annotations, encryption (V1-V5); new wiki
  pages for every feature.
- 2026-09-26: Initial wiki, text-only document model (`PdfDocument`,
  `PdfPage`, `PdfTextRun`) and `PdfView` (page, zoom, theme, Vello text).
