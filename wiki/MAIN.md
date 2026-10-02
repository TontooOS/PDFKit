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
| PdfEditor | [PdfEditor.md](PdfEditor.md) | Rewriting text runs and saving in place |
| IncrementalUpdate | [IncrementalUpdate.md](IncrementalUpdate.md) | Appending edited objects to an existing file |
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

- 2026-10-02: Real text editing. `PdfEditor` splices new bytes at a
  run's `OpSpan`, re-encodes only the changed content streams and saves
  as an incremental update (`UpdateWriter`); `Document::open` and
  `PdfEditor::save_to` match the API the Preview app already expected.
  Encrypted files are refused with `EncryptedEdit`. New `editdemo`
  example; poppler reads and renders the result.
- 2026-10-02: Fixed two classic-xref-table bugs that had been masked by
  the object-scan fallback. Entry type was read from the generation
  field, so no table entry was ever marked in use, and the `xref`
  keyword was not skipped when following `/Prev`. Every table-based file
  was being recovered by scanning, which kept the *first* copy of each
  object and silently ignored incremental updates.
- 2026-10-02: Editor foundation. Content tokens carry byte ranges end
  to end: `OpSpan` on text runs (pointing at their own string, never
  the `TJ` array), paths and images (pointing at the active `cm`);
  `PdfPage` keeps `content` plus `content_segments` mapping every
  `/Contents` entry to its object number. Form XObject bodies are
  addressable too. This is what byte-exact edits and the incremental
  save in the next milestones build on. Read-only until then.
- 2026-10-02: Stacked/scrolled viewers place run text correctly again
  (run origins are mapped at draw time instead of being cached with
  the first `measure()` position).
- 2026-09-28: Visual compare loop fully green (105/105): rotated
  text and page `/Rotate`, spec-correct LZW codes, calibrated CMYK
  table, CropBox frames, stencil polarity, annotation verdicts.
- 2026-09-26: Full standard pass M1-M8: filters, xref streams, graphics,
  transparency, fonts, images, annotations, encryption (V1-V5); new wiki
  pages for every feature.
- 2026-09-26: Initial wiki, text-only document model (`PdfDocument`,
  `PdfPage`, `PdfTextRun`) and `PdfView` (page, zoom, theme, Vello text).
