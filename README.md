# PDFKit

Native PDF document model and viewer for TontooOS.

Parses real PDF files (header, xref table, page tree, FlateDecode content
streams) into positioned text runs and renders them as a TontooUI `View`
through the shared `FontSystem` (SF Pro, Parley layout, Vello glyphs).
Nothing is rasterized to a bitmap, so the editor milestone can select and
edit text in place.

v0.1 renders text only (`BT`/`ET`, `Tf`, `Tj`, `TJ`, `Tm`, `Td`, ...).
See the [wiki](wiki/MAIN.md) for the feature pages.

## Made for TontooOS

Explore more at https://github.com/TontooOS/Libs

## Adding to Your Project

Add to your `Cargo.toml`:

```toml
[dependencies]
sdk = { path = "/Library/System/sdk", features = ["PDFKit", "TontooUI"] }
```

Then at the crate root:

```rust
sdk::preinclude!();

use PDFKit::{PdfDocument, PdfView};

let doc = PdfDocument::load_file("/path/to/file.pdf").unwrap();
let view = PdfView::new(doc);
```

## License

TCL v26.1