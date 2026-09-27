# Baseline (2026-09-27, scale 2, poppler reference)

First full loop run. Harness: `COMPARE.md`. Failing elements are
the work queue; fixers take them in batches (max 25 per round).

## Totals

- Elements: 105, passed 39, failed 66.

| Kind | Passed | Failed |
|---|---|---|
| annot | 0 | 9 |
| clip | 3 | 0 |
| color | 7 | 4 |
| crop | 0 | 2 |
| filter | 1 | 3 |
| form | 4 | 0 |
| gstate | 3 | 0 |
| image | 0 | 6 |
| info | 0 | 1 |
| outline | 0 | 2 |
| path | 15 | 0 |
| rotate | 0 | 2 |
| shading | 2 | 0 |
| text | 0 | 37 |
| transform | 4 | 0 |

Failed IDs: E001-E069, E076-E094, E095-E101, E102-E105
(E070-E075 pass: gstate/shading; E095-E101 are outline/info/filter
label elements; E102-E105 are crop/rotate structural).

## Suspected clusters (for fixers, verify with strips)

1. Text vertical origin (E001-E037 and friends): our text sits
   roughly 15 px too low at scale 2. Suspect `run_origin` in
   `src/view.rs` (baseline-minus-`font_size` estimate) versus
   CoreText-measured ascent. Fix there first; it unlocks all text.
2. Images (6): check placement/colormaps against strips.
3. Annotations (9): markup paint positions and link borders.
4. Colors (4): spot/indexed/cal/lab/icc swatches vs poppler.
5. Filters (3 on mixed-filter page): content order or spacing.
6. Crop (2): coordinate frames differ (we crop, poppler full).
7. Rotate (2): `/Rotate` unsupported by design so far.
8. Outline/info (3): label presence in reference.

## Reference warnings (expected, keep)

- Non-embedded Identity-H CID font (poppler cannot render it; we
  render via ToUnicode, labeled in-PDF).
- Dummy ICC profile (poppler falls back to `/Alternate`, same as us).
