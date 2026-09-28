# PdfView

`PdfView` is a TontooUI `View` that renders one `PdfDocument` page from
its vector/text item model: text runs, paths, gradients, images and
annotations. Text draws only through `draw_layout` with logical px and
a `layout_scale` cache, so it stays crisp on DPI changes (see the
TontooUI crisp text rules). Nothing is rasterized to a bitmap.

The paper stays white in both themes like in standard viewers while
PDF colors are honored; the window chrome around the page follows the
theme instead.

## Type

```rust
pub struct PdfView { /* doc, page_no, zoom, theme, rect, runs, images */ }
```

Default state is page 0 at 1x zoom in dark mode. Image uploads are
cached per page and item; text layouts rebuild on page, zoom, theme
or display-scale changes.

## Constructors

```rust
pub fn new(doc: PdfDocument) -> Self
```

Creates a view over `doc`, showing page 0 at 1x zoom.

## Builder and state methods

| Function | Signature | Behavior |
|---|---|---|
| `page_count` | `page_count(&self) -> usize` | Number of pages in the document |
| `current_page` | `current_page(&self) -> usize` | Shown page, zero-based |
| `set_page` | `set_page(&mut self, index: usize)` | Show page `index`; out-of-range clamps |
| `next_page` | `next_page(&mut self)` | Advance one page, stays on the last page |
| `prev_page` | `prev_page(&mut self)` | Go back one page, stays on the first page |
| `set_zoom` | `set_zoom(&mut self, zoom: f32)` | Zoom factor, clamped to `0.25..=8.0` |
| `zoom` | `zoom(&self) -> f32` | Current zoom factor |
| `set_theme` | `set_theme(&mut self, mode: ThemeMode)` | Dark or light chrome (paper stays white) |
| `rect` | `rect(&self) -> (f32, f32, f32, f32)` | Placed rect in logical px |

## View protocol

`PdfView` implements `tontooui::elements::layout::View`:

- `measure` returns the zoomed page size (`width * zoom`, `height * zoom`,
  swapped for `/Rotate` 90/270).
- `place` stores the page rect at the given origin.
- `draw` fills the paper, replays items (paths with clip stack,
  gradients, images, text) and paints annotations on top.
- Rotated text (`cm` rotation) draws through a rigid Vello glyph
  transform about the glyph origin; page `/Rotate` maps every item
  into the rotated display frame. Page size follows the CropBox.

## Usage / Example

```rust
use pdfkit::{PdfDocument, PdfView};
use tontooui::elements::layout::{View, VStack};
use tontooui::theme::ThemeMode;

let doc = PdfDocument::load_file("/path/to/file.pdf").unwrap();
let mut view = PdfView::new(doc);
view.set_theme(ThemeMode::Dark);
view.set_zoom(1.5);
view.next_page();

let stack = VStack::new().child(view);
```

## Cross References

- [PdfDocument.md](PdfDocument.md) – the parsed page model this view renders
- [Graphics.md](Graphics.md) – paths, gradients and transparency it draws
- [Images.md](Images.md) – image placement and masks it draws
- [Annotations.md](Annotations.md) – markup it paints on top
- [Language.md](Language.md) – page counter and zoom label strings
