# Baseline (2026-09-27, scale 2, poppler reference)

Visual loop per `COMPARE.md`. Failing elements are the work queue.

## Totals

- Elements: 105, passed 74, failed 31.

| Kind | Passed | Failed |
|---|---|---|
| annot | 3 | 6 |
| clip | 3 | 0 |
| color | 7 | 4 |
| crop | 0 | 2 |
| filter | 0 | 4 |
| form | 4 | 0 |
| gstate | 3 | 0 |
| image | 0 | 6 |
| info | 0 | 1 |
| outline | 0 | 2 |
| path | 15 | 0 |
| rotate | 0 | 2 |
| shading | 2 | 0 |
| text | 33 | 4 |
| transform | 4 | 0 |

Failed IDs: E022 E023 E025 E026 E062 E063 E064 E068 E076 E077
E078 E079 E080 E081 E086 E087 E088 E089 E090 E092 E095 E096
E097 E098 E099 E100 E101 E102 E103 E104 E105

## History

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
