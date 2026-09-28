# Annotations

PDFKit parses page annotations, bookmark outlines and document
metadata, and the view paints markup annotations on top of the page.

## Annotations

| Subtype | Behavior |
|---|---|
| `Link` | `/Dest` and `/A` actions: `GoTo`, `URI`, `Named` |
| `Highlight` | Quad fills at 35% alpha, `/C` color |
| `Underline`, `StrikeOut` | Edge and mid lines from quads or rect |
| `Square`, `Circle` | Stroked rect and ellipse, `/Border`/`/BS` width |
| `Ink` | Stroked `/InkList` polylines |
| `Text` | Filled note rect in `/C` (poppler draws a detailed icon) |
| Other | Parsed (`contents`, `rect`, `color`) for the editor |

Annotations without `/C` fall back to black (poppler/Acrobat
behavior for colorless link borders).

Explicit destinations resolve page references and page numbers to
zero-based indices; named destinations resolve through `/Names`
`/Dests` trees and old-style catalog `/Dests`.

## Outlines and metadata

`PdfDocument::outlines` returns the bookmark tree with resolved page
targets. `PdfDocument::info` returns `/Title`, `/Author`, `/Subject`,
`/Keywords`, `/Creator`, `/Producer` and the raw date strings.
Text strings decode as UTF-16BE with BOM or PDFDocEncoding.

## Marked content

`BMC`/`BDC`/`EMC` sections become `BeginMarked`/`EndMarked` items
carrying the tag and property name (`<dict>` for inline dicts).
`MP`/`DP` carry no items. Compatibility `BX`/`EX` sections are
skipped entirely.

## Types

```rust
pub struct Annotation { pub rect: [f32; 4], pub subtype: String, pub contents: Option<String>, pub color: Option<Rgb>, pub border_width: f32, pub quads: Vec<[f32; 8]>, pub ink: Vec<Vec<(f32, f32)>>, pub target: Option<LinkTarget> }
```

```rust
pub enum LinkTarget { Page(usize), Uri(String), Named(String) }
```

```rust
pub struct Outline { pub title: String, pub target: Option<LinkTarget>, pub children: Vec<Outline> }
```

```rust
pub struct DocInfo { /* title, author, subject, keywords, creator, producer, dates */ }
```

## Usage / Example

```rust
use pdfkit::{LinkTarget, PdfDocument};

let doc = PdfDocument::load_file("/path/to/file.pdf").unwrap();
for annot in &doc.page(0).unwrap().annotations {
  if let Some(LinkTarget::Uri(url)) = &annot.target {
    println!("{url}");
  }
}
```

## Cross References

- [PdfDocument.md](PdfDocument.md) – outlines and info accessors
- [PdfView.md](PdfView.md) – markup painting order
- [Language.md](Language.md) – link-related strings (none yet)
