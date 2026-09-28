# Baseline (2026-09-28, scale 2, poppler reference, WSL render)

Visual loop per `COMPARE.md`. All 105 elements pass.

## Totals

- Elements: 105, passed 105, failed 0.

| Kind | Passed | Failed |
|---|---|---|
| annot | 9 | 0 |
| clip | 3 | 0 |
| color | 11 | 0 |
| crop | 2 | 0 |
| filter | 4 | 0 |
| form | 4 | 0 |
| gstate | 3 | 0 |
| image | 6 | 0 |
| info | 1 | 0 |
| outline | 2 | 0 |
| path | 15 | 0 |
| rotate | 2 | 0 |
| shading | 2 | 0 |
| text | 37 | 0 |
| transform | 4 | 0 |

Failed IDs: none.

## History

- 105/0: full green. LZW Clear/EOD swapped to the spec
  assignment (256 clear, 257 EOD; decoder, test encoder and
  coverage generator were consistently inverted, so real-world
  LZW streams decoded empty). DeviceCMYK converts through a
  calibrated SWOP-like 5x5x5x5 lookup table
  (`src/cmyk_lut.rs`) instead of the naive formula. E038 passes
  via 50%-gray ink counting, E101 via spec-conformant LZW,
  E062-E064 via the table. No regressions.
- 102/3: remaining failures E062 E063 E064 (CMYK needs color
  management, fixed next).
- 100/5: rotated text (`cm` 30 degrees, E026) via a rigid Vello
  glyph transform about the glyph origin; page `/Rotate` mapped
  into the rotated display frame (E104/E105, manifest rects in
  that frame); SMask single-decode fix (E079); CropBox frames
  (`ours_rect`, `absent` verdict for E102/E103); text verdicts
  for E038 and the filter page (E098-E101).
- 91/14: annotations + outline/info batch (10 fixes). Renderer:
  colorless annots fall back to black (E086 link border) and `Text`
  annots paint a note marker (E094) instead of being skipped.
  Harness: manifest `"verdict":"text"` judges text-dominated
  annot/outline/info crops by ink edges (fonts differ by design:
  SF Pro vs DejaVu). E086-E090, E092, E094-E097 flip to PASS, no
  regressions. Known quirk: poppler rasterizes the E088 highlight
  quad as a bowtie; we paint the spec-straight translucent rect
  (Firefox agrees), so the element is position-judged.
- 81/24: stencil mask polarity fixed (`decode_mask_alpha` paints
  0-bits under the default `/Decode [0 1]`, matching poppler and
  Firefox); E080 flips to PASS, E081 stays PASS, no regressions.
- 80/25: declared font Widths, ImageMask boolean handling and
  blocky (`/Interpolate`) image quality; E080 still failed on
  stencil polarity (mean 22.9).

- 39/66: first run (text used pixel diff across DejaVu vs SF Pro).
- 59/46: text vertical origin calibrated to measured CoreText
  baseline (`line_baseline` in `src/view.rs`).
- 74/31: text verdict switched to ink edges (left/top/bottom)
  instead of coverage mass; non-text threshold mean <= 8.0.

## Notable clusters (for fixers, verify with strips)

1. Text E022 (both crops empty: manifest rect misses the ink),
   E023/E025/E026 (partial ink: suspect TJ/advance/rotated runs).
2. Images (6): placement/colormaps against strips.
3. Annotations (6 of 9): markup paint positions, link borders.
4. Colors (4): spot/indexed/cal/lab/icc swatches vs poppler.
5. Filters (4 on mixed-filter page): content order or spacing.
6. Outline/info (3): label presence in reference.
7. Crop (2): coordinate frames differ (we crop, poppler full).
8. Rotate (2): `/Rotate` unsupported by design so far.

## Reference warnings (expected, keep)

- Non-embedded Identity-H CID font (poppler cannot render it; we
  render via ToUnicode, labeled in-PDF).
- Dummy ICC profile (poppler falls back to `/Alternate`, same as us).
