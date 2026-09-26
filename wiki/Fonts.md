# Fonts

The `font` module decodes text bytes and measures advances for simple
fonts, ToUnicode CMaps and CID fonts. Glyph outlines stay with the
system fonts (SF Pro); embedding parsing is not needed for layout.

## Base encodings

| Encoding | Behavior |
|---|---|
| `WinAnsiEncoding` | Latin-1 with the `0x80-0x9F` controls mapped |
| `MacRomanEncoding` | Full MacRoman table |
| `StandardEncoding` | ASCII plus the Adobe high-half vector |
| Unknown names | WinAnsi fallback |

`/Differences` arrays override single codes by Adobe glyph name
(single letters plus a curated Latin, punctuation, currency,
ligature and math subset; unknown names map to empty).

## ToUnicode CMaps

| Construct | Behavior |
|---|---|
| `codespacerange` | 1-4 byte code splitting, longest match |
| `bfchar` | Single-code to UTF-16BE string |
| `bfrange` base | Offset added to the last UTF-16 unit |
| `bfrange` array | One string per code |
| Surrogate pairs | Decoded via UTF-16 |

Unmapped codes yield U+FFFD. Identity CID fonts without ToUnicode
decode to U+FFFD (documented gap: no embedded outlines to map).

## Widths

Simple fonts read `/FirstChar`, `/LastChar` and `/Widths`; CID fonts
read `/DW` and `/W` (single codes and ranges). Missing entries fall
back to `/MissingWidth` or 500/1000. Advances combine widths with
`Tc`, `Tw` and `TJ` adjustments.

Bold and italic come from the `/BaseFont` name and the
`/FontDescriptor` `/Flags` bits (18 bold, 6 italic).

## Types

```rust
pub struct FontDecoder { /* kind, widths, default_width */ }
```

```rust
pub fn parse_cmap(data: &[u8]) -> Result<CMap>
```

```rust
pub fn codes(&self, bytes: &[u8]) -> Vec<u32>
```

```rust
pub fn text_of(&self, code: u32) -> String
```

```rust
pub struct PdfTextRun { pub text: String, pub x: f32, pub y: f32, pub font_size: f32, pub bold: bool, pub font_name: String, pub color_rgb: [f32; 3], pub dir_x: f32, pub dir_y: f32, pub alpha: f32 }
```

## Usage / Example

```rust
use pdfkit::{FontDecoder, parse_cmap};

let cmap = parse_cmap(b"1 beginbfchar <41> <0042> endbfchar").unwrap();
let decoder = FontDecoder { kind: pdfkit::DecoderKind::CMap(cmap), widths: Default::default(), default_width: 500.0 };
assert_eq!(decoder.decode(&[0x41]), "B");
```

## Cross References

- [Graphics.md](Graphics.md) – text showing operators using the decoder
- [PdfDocument.md](PdfDocument.md) – font resources per page
