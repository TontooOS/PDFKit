# PdfView

`PdfView` is a TontooUI `View` that renders one `PdfDocument` page as
positioned text runs. It draws only through `draw_layout` with logical px
and a `layout_scale` cache, so text stays crisp on DPI changes (see the
TontooUI crisp text rules). Nothing is rasterized to a bitmap: every run
keeps its PDF coordinates for the editor milestone.

v0.1 maps all runs to the theme text color. PDF color operators, images,
paths and annotations follow in later milestones.

## Type

```rust
pub struct PdfView { /* doc, page_no, zoom, theme, rect, layouts */ }
```

Default state is page 0 at 1x zoom in dark mode. Colors follow AGENTS.md:
dark background `#1b2022` with text `#d8d9d9`, light background `#ffffff`
with text `#272727`.

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
| `set_theme` | `set_theme(&mut self, mode: ThemeMode)` | Dark or light page colors |
| `rect` | `rect(&self) -> (f32, f32, f32, f32)` | Placed rect in logical px |

## View protocol

`PdfView` implements `tontooui::elements::layout::View`:

- `measure` returns the zoomed page size (`width * zoom`, `height * zoom`).
- `place` stores the page rect at the given origin.
- `draw` fills the page background and draws one Parley layout per run
  at its PDF position (Y flipped from bottom-left to top-left origin).

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
- [Language.md](Language.md) – page counter and zoom label strings
