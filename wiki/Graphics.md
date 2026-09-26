# Graphics

The `graphics` module interprets content streams into `PageItem`
values: text runs with full text matrices, paths with paint, axial
and radial shadings, placed images and structure markers.

## Matrices

```rust
pub struct Matrix { pub a: f32, pub b: f32, pub c: f32, pub d: f32, pub e: f32, pub f: f32 }
```

Row-vector convention (`x' = a*x + c*y + e`). `concat` applies the
left matrix first (`CTM' = M x CTM` for `cm`).

## State and operators

| Operator group | Operators | Behavior |
|---|---|---|
| Stack | `q`, `Q` | Save/restore; `Save`/`Restore` items bound clip lifetime |
| Transform | `cm` | Concatenates onto the CTM |
| Line | `w`, `J`, `j`, `M`, `d` | Width, cap, join, miter, dash array and phase |
| Path | `m`, `l`, `c`, `v`, `y`, `h`, `re` | Segments in user space |
| Paint | `S`, `s`, `f`, `F`, `f*`, `B`, `B*`, `b`, `b*`, `n` | Fill/stroke/close; path consumed |
| Clip | `W`, `W*` | Pending clip applied by the next paint operator |
| Color | `G`, `g`, `RG`, `rg`, `K`, `k` | Device gray, RGB, CMYK (naive conversion) |
| Color | `CS`, `cs`, `SC`, `sc`, `SCN`, `scn` | Named spaces resolve via resources |
| Text state | `Tf`, `Tc`, `Tw`, `Tz`, `TL`, `Ts`, `Tr` | Font, spacing, scale, leading, rise, mode |
| Text position | `Tm`, `Td`, `TD`, `T*` | Text line matrix updates |
| Text show | `Tj`, `TJ`, `'`, `"` | Runs with `Trm` origin from font, Tlm and CTM |
| Special | `gs` | ExtGState line attributes and constant alpha |
| Special | `Do` | Form/image XObjects (see [Images.md](Images.md)) |
| Special | `sh` | Axial/radial shadings to gradient items |
| Special | `BMC`, `BDC`, `EMC`, `MP`, `DP` | Structure markers, no paint |
| Special | `BX`, `EX` | Compatibility sections skipped |

Render mode 3 (invisible text, common for OCR layers) emits no run.
Stroked text modes paint filled (documented gap); text Stroking
beyond fill is not rendered.

## Colors and transparency

Device spaces map directly to sRGB. Special spaces (`Separation`,
`DeviceN`, `Indexed`, `ICCBased`, `CalGray`, `CalRGB`, `Lab`)
evaluate through resources; see [PdfDocument.md](PdfDocument.md).
`ca`/`CA` bake into paint alpha; blend modes, overprint and soft
masks parse but render as normal opaque paint.

Axial (`ShadingType` 2) and radial (3) shadings sample their Type
0/2/3 functions into Vello gradient stops. Mesh shadings (4-7),
function-based (1) and tiling patterns parse but do not paint.

## Types

```rust
pub enum PageItem { Text(PdfTextRun), Path(PathItem), Gradient(GradientItem), Image(PlacedImage), Pattern(String), Skipped(String), BeginMarked(Marked), EndMarked, Save, Restore }
```

```rust
pub fn interpret<R: ResourceProvider>(content: &[u8], res: R) -> Result<Vec<PageItem>>
```

```rust
pub fn interpret_with<R: ResourceProvider>(content: &[u8], res: R, base_ctm: Matrix, depth: u32) -> Result<Vec<PageItem>>
```

Form nesting past `MAX_FORM_DEPTH` (8) yields `Skipped`.

## Usage / Example

```rust
use pdfkit::{MapResources, interpret};

let items = interpret(b"1 0 0 rg 10 20 30 40 re f", MapResources::default()).unwrap();
```

## Cross References

- [Fonts.md](Fonts.md) – text decoding and widths behind the runs
- [Images.md](Images.md) – XObject resolution and placement
- [PdfView.md](PdfView.md) – how items paint into a Vello scene
