# Compare loop: Firefox/poppler reference vs PDFKit

Repeatable visual regression loop. All binary outputs live in
`target/compare/` (gitignored); only the generators, harness,
manifest sources and reports are committed.

## One-time setup

Tools: WSL `archlinux` with `pdftoppm` (poppler) for reference
PNGs; Windows with GPU for our renders. Scales must match:
`--scale S` renders `mediabox_pts * S` pixels; reference uses
`-r R` dpi with `R = 72 * S` (S=2 matches `-r 144`).

## Full run (scale 2)

```powershell
# 1. Coverage PDF + manifest + self-check (WSL):
wsl -d archlinux -- bash -lc 'cd /mnt/c/Users/arlo1/Documents/TontooLibs/PDFKit && cargo run --example coverage'
# -> target/compare/coverage.pdf + coverage.json (105 elements)

# 2. Reference PNGs (WSL):
wsl -d archlinux -- bash -lc 'cd /mnt/c/Users/arlo1/Documents/TontooLibs/PDFKit/target/compare && rm -f ref/page-*.png && pdftoppm -png -r 144 coverage.pdf ref/page'
```

Rename `ref/page-N.png` (1-based) to `ref/pageN.png` (0-based) so
both sides share names:

```powershell
$i = 1
Get-ChildItem target/compare/ref -Filter 'page-*.png' | Sort-Object Name | ForEach-Object {
  Rename-Item $_.FullName ('page{0}.png' -f ($i - 1)); $i++
}
```

```powershell
# 3. Our PNGs (Windows, needs GPU):
cargo run --example renderpng -- target/compare/coverage.pdf target/compare/ours --scale 2

# 4. Diff (WSL or Windows):
cargo run --example diffcov -- target/compare/coverage.json target/compare/ref target/compare/ours --scale 2
```

## How diffcov judges

- Manifest `rect` is PDF points `[x0, y0, x1, y1]`; crops are
  `rect * S` pixels (PNG origin top-left, Y flipped).
- Reference crop auto-aligns by searching +-4 px for the lowest
  mean absolute channel difference.
- Non-text kinds PASS at mean <= 8.0 (tolerates AA fringes and
  1 px shifts; real bugs score far higher).
- Text uses profiles, not pixels: the reference renders DejaVu
  while we render SF Pro, so glyph shapes never match. PASS needs
  ink coverage > 0.5%, coverage ratio within 60%, vertical center
  within 8 px and horizontal center within 12 px.
- Failing elements get `ours/diffstrip_<id>.png`
  (reference | ours | abs-diff). Exit code 1 on any failure.

## Known caveats (not bugs in the harness)

- Reference warnings on generation are expected: non-embedded
  Identity-H CID font, dummy ICC profile (poppler falls back to
  `/Alternate`, same as us).
- CropBox page: our PNG is cropped to the box, poppler keeps full
  size; element crops may go out of bounds (FAIL with reason).
- `/Rotate` page: unsupported by design so far (labeled in-PDF).
- Text origin: our baseline-to-top estimate is suspect (see
  BASELINE.md); fixers calibrate against CoreText-measured ascent.
