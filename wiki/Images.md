# Images

The `image` module decodes raster XObjects plus form XObject
recursion. Images stay placed vectors (unit square through the CTM),
never flattened page bitmaps.

## Image XObjects

| Feature | Behavior |
|---|---|
| Sample depths | 1, 2, 4, 8 and 16 bits per component |
| Device spaces | Fast gray, RGB and CMYK paths |
| Special spaces | Mapped per pixel through resources |
| `/Decode` | Per-component inversion and scaling |
| `/Interpolate` | Stored as a hint on the decoded image |
| DCTDecode | JPEG decoded with the `image` crate |
| JPX, CCITT, JBIG2 | `Skipped` (documented gap) |
| `/SMask` | Soft-mask alpha planes applied to RGBA |
| `/ImageMask` | Stencil painted with the current fill color; 0-bits paint under the default `/Decode [0 1]` |
| Constant alpha | `ca` baked into the alpha channel |

Width, height and sample counts are capped (16384 px, buffer checks)
so corrupt dicts cannot allocate wildly.

## Inline images

`BI..EI` blocks parse abbreviated keys (`W`, `H`, `BPC`, `CS`,
`F`, `G`/`RGB`/`CMYK`/`I` spaces) and decode through the same path
as image XObjects.

## Form XObjects

`Do` splices form content into the item stream with the form
`/Matrix` concatenated onto the current CTM, clipped to `/BBox`
inside a `Save`/`Restore` frame. Nesting past `MAX_FORM_DEPTH` (8)
yields `Skipped`, which also guards cyclic forms.

## Types

```rust
pub struct DecodedImage { pub width: u32, pub height: u32, pub rgba: Vec<u8>, pub interpolate: bool }
```

```rust
pub struct PlacedImage { pub image: DecodedImage, pub ctm: Matrix }
```

```rust
pub fn decode_samples(dict: &PdfValue, samples: &[u8], map: &dyn Fn(&[f32]) -> Option<Rgb>) -> Option<DecodedImage>
```

```rust
pub fn decode_jpeg(data: &[u8]) -> Option<DecodedImage>
```

## Usage / Example

```rust
use pdfkit::{PdfDocument, PlacedImage, PageItem};

let doc = PdfDocument::load_file("/path/to/file.pdf").unwrap();
for item in &doc.page(0).unwrap().items {
  if let PageItem::Image(placed) = item {
    println!("{}x{}", placed.image.width, placed.image.height);
  }
}
```

## Cross References

- [Graphics.md](Graphics.md) – `Do` dispatch and form framing
- [DocumentStructure.md](DocumentStructure.md) – stream filters reused here
- [PdfView.md](PdfView.md) – unit-square mapping into the scene
