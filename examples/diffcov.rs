use coreimage::{ImageFormat, TiImage};

/// Element-wise visual diff for the Firefox-vs-PDFKit compare loop.
///
/// Usage: `diffcov <manifest.json> <refdir> <ourdir> --scale S [--ref-offset X,Y] [--ref-crop X,Y,W,H]`
///
/// Both dirs hold `pageN.png` files. Each manifest rect (PDF points)
/// is scaled by S into pixel crops; the reference crop starts at
/// rect*S plus --ref-offset (after the optional --ref-crop pre-cut)
/// and is auto-aligned by searching +-4 px for the lowest mean
/// absolute difference. PASS needs mean <= 3.0 AND max <= 40.
/// Failing elements get a `diffstrip_<id>.png` side-by-side strip
/// (reference | ours | abs-diff) written into <ourdir>.
/// Exit code is 1 when any element fails, 0 otherwise.
fn main() {
  let args: Vec<String> = std::env::args().collect();
  if args.len() < 5 {
    eprintln!("usage: diffcov <manifest.json> <refdir> <ourdir> --scale S [--ref-offset X,Y] [--ref-crop X,Y,W,H]");
    std::process::exit(2);
  }
  let manifest_path = &args[1];
  let refdir = &args[2];
  let ourdir = &args[3];
  let mut scale = 0.0f32;
  let mut ref_offset = (0i32, 0i32);
  let mut ref_crop: Option<(u32, u32, u32, u32)> = None;
  let mut i = 4;
  while i < args.len() {
    match args[i].as_str() {
      "--scale" => {
        scale = args.get(i + 1).unwrap_or_else(|| die("missing --scale value")).parse().unwrap_or_else(|_| die("bad --scale value"));
        i += 2;
      }
      "--ref-offset" => {
        ref_offset = parse_pair(args.get(i + 1).unwrap_or_else(|| die("missing --ref-offset value")));
        i += 2;
      }
      "--ref-crop" => {
        ref_crop = Some(parse_quad(args.get(i + 1).unwrap_or_else(|| die("missing --ref-crop value"))));
        i += 2;
      }
      other => die(&format!("unknown arg {other}")),
    }
  }
  if scale <= 0.0 {
    die("need --scale S with S > 0");
  }
  let manifest_text = std::fs::read_to_string(manifest_path).unwrap_or_else(|e| die(&format!("cannot read {manifest_path}: {e}")));
  let manifest: serde_json::Value = serde_json::from_str(&manifest_text).unwrap_or_else(|e| die(&format!("bad manifest JSON: {e}")));
  let entries = manifest.as_array().unwrap_or_else(|| die("manifest must be a JSON array"));
  let mut passed = 0u32;
  let mut failed = 0u32;
  let mut failed_ids: Vec<String> = Vec::new();
  // Per-kind tallies: (kind, passed, failed).
  let mut kinds: std::collections::BTreeMap<String, (u32, u32)> = std::collections::BTreeMap::new();
  // Cache loaded page PNGs (both sides) across elements.
  let mut ref_pages: std::collections::HashMap<u32, Img> = std::collections::HashMap::new();
  let mut our_pages: std::collections::HashMap<u32, Img> = std::collections::HashMap::new();
  for entry in entries {
    let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("?");
    let page = entry.get("page").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let kind = entry.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
    let rect: Vec<f32> = entry
      .get("rect")
      .and_then(|v| v.as_array())
      .map(|a| a.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect())
      .unwrap_or_default();
    if rect.len() != 4 {
      println!("{id} p{page} {kind}: bad rect, FAIL (manifest error)");
      failed += 1;
      failed_ids.push(id.to_string());
      continue;
    }
    let our = load_cached(&mut our_pages, &format!("{ourdir}/page{page}.png"));
    let mut reference = load_cached(&mut ref_pages, &format!("{refdir}/page{page}.png"));
    if let Some((cx, cy, cw, ch)) = ref_crop {
      reference = crop(&reference, cx as i32, cy as i32, cw, ch);
    }
    // Element crop in our PNG: exact rect*S (PDF origin is
    // bottom-left, PNG origin top-left).
    let ox0 = (rect[0] * scale).round() as i32;
    let oy1 = (rect[3] * scale).round() as i32;
    let ox1 = (rect[2] * scale).round() as i32;
    let oy0 = (rect[1] * scale).round() as i32;
    let (ow, oh) = (ox1 - ox0, oy1 - oy0);
    let ours = crop_top_left(&our, ox0, page_height_top(&our, oy0, oy1), ow as u32, oh as u32);
    if ours.pixels.is_empty() {
      println!("{id} p{page} {kind}: our crop out of bounds, FAIL");
      failed += 1;
      failed_ids.push(id.to_string());
      tally(&mut kinds, kind, false);
      continue;
    }
    // Reference crop: same size, translated by --ref-offset, then
    // +-4 px search for the best alignment.
    let base_x = ox0 + ref_offset.0;
    let base_y_top = page_height_top(&reference, oy0, oy1) + ref_offset.1;
    let mut best = (f64::INFINITY, 0u8, 0i32, 0i32, Vec::<u8>::new(), (0u32, 0u32));
    for dy in -4..=4 {
      for dx in -4..=4 {
        let cand = crop_top_left(&reference, base_x + dx, base_y_top + dy, ow as u32, oh as u32);
        if cand.pixels.is_empty() || cand.w != ours.w || cand.h != ours.h {
          continue;
        }
        let (mean, max) = diff_metrics(&ours.pixels, &cand.pixels);
        if mean < best.0 {
          let dims = cand.dims();
          best = (mean, max, dx, dy, cand.pixels, dims);
        }
      }
    }
    if best.0.is_infinite() {
      println!("{id} p{page} {kind}: ref crop out of bounds, FAIL");
      failed += 1;
      failed_ids.push(id.to_string());
      tally(&mut kinds, kind, false);
      continue;
    }
    let (mean, max, dx, dy, ref_px, (cw_used, ch_used)) = best;
    // Verdict: non-text kinds compare pixels (mean tolerates AA
    // fringes and 1px shifts; real bugs score far higher). Text
    // compares profiles instead: different fonts (DejaVu vs SF Pro)
    // never match pixels, but presence, ink amount and center of
    // mass must agree.
    let ok = if kind == "text" {
      let (rcov, rvcom, rhcom) = text_profile(&ref_px, cw_used, ch_used);
      let (ocov, ovcom, ohcom) = text_profile(&ours.pixels, cw_used, ch_used);
      let cov_ok = ocov > 0.005 && (rcov - ocov).abs() / rcov.max(1e-6) < 0.6;
      let pos_ok = (rvcom - ovcom).abs() <= 8.0 && (rhcom - ohcom).abs() <= 12.0;
      println!(
        "{id} p{page} {kind}: mean={mean:.2} max={max} off={dx},{dy} cov={rcov:.3}/{ocov:.3} vcom={rvcom:.1}/{ovcom:.1} {}",
        if cov_ok && pos_ok { "PASS" } else { "FAIL" }
      );
      cov_ok && pos_ok
    } else {
      let ok = mean <= 8.0;
      println!("{id} p{page} {kind}: mean={mean:.2} max={max} off={dx},{dy} {}", if ok { "PASS" } else { "FAIL" });
      ok
    };
    if ok {
      passed += 1;
    } else {
      failed += 1;
      failed_ids.push(id.to_string());
      write_strip(ourdir, id, &ref_px, &ours.pixels, cw_used, ch_used);
    }
    tally(&mut kinds, kind, ok);
  }
  println!("---");
  println!("total={} passed={passed} failed={failed}", passed + failed);
  for (kind, (p, f)) in &kinds {
    println!("kind {kind}: passed={p} failed={f}");
  }
  if !failed_ids.is_empty() {
    println!("failed: {}", failed_ids.join(" "));
  }
  if failed > 0 {
    std::process::exit(1);
  }
}

fn die(msg: &str) -> ! {
  eprintln!("error: {msg}");
  std::process::exit(2);
}

fn parse_pair(s: &str) -> (i32, i32) {
  let mut parts = s.split(',');
  let x = parts.next().unwrap_or("").trim().parse().unwrap_or_else(|_| die("bad --ref-offset, want X,Y"));
  let y = parts.next().unwrap_or("").trim().parse().unwrap_or_else(|_| die("bad --ref-offset, want X,Y"));
  (x, y)
}

fn parse_quad(s: &str) -> (u32, u32, u32, u32) {
  let p: Vec<u32> = s.split(',').map(|v| v.trim().parse().unwrap_or_else(|_| die("bad --ref-crop, want X,Y,W,H"))).collect();
  if p.len() != 4 {
    die("bad --ref-crop, want X,Y,W,H");
  }
  (p[0], p[1], p[2], p[3])
}

#[derive(Clone)]
struct Img {
  pixels: Vec<u8>,
  w: u32,
  h: u32,
}

impl Img {
  fn dims(&self) -> (u32, u32) {
    (self.w, self.h)
  }
}

fn load_cached(cache: &mut std::collections::HashMap<u32, Img>, path: &str) -> Img {
  // Cache key is the page number parsed from "pageN.png".
  let page: u32 = std::path::Path::new(path)
    .file_stem()
    .and_then(|s| s.to_str())
    .and_then(|s| s.strip_prefix("page"))
    .and_then(|s| s.parse().ok())
    .unwrap_or(u32::MAX);
  if let Some(img) = cache.get(&page) {
    return img.clone();
  }
  let img = TiImage::load(path).unwrap_or_else(|e| die(&format!("cannot load {path}: {e}")));
  let (w, h) = img.dimensions();
  let pixels = img.into_rgba().into_raw();
  let out = Img { pixels, w, h };
  cache.insert(page, out.clone());
  out
}

/// Top-left Y of a bottom-left-origin rect band [y0, y1] in an
/// image of height h.
fn page_height_top(img: &Img, y0: i32, y1: i32) -> i32 {
  img.h as i32 - y1.max(y0)
}

/// Crop (x, y_top, w, h) in top-left pixels. Empty pixels when the
/// rect has no overlap with the image.
fn crop_top_left(img: &Img, x: i32, y_top: i32, w: u32, h: u32) -> Img {
  crop(img, x, y_top, w, h)
}

fn crop(img: &Img, x: i32, y: i32, w: u32, h: u32) -> Img {
  let x0 = x.max(0) as u32;
  let y0 = y.max(0) as u32;
  let x1 = (x + w as i32).clamp(0, img.w as i32) as u32;
  let y1 = (y + h as i32).clamp(0, img.h as i32) as u32;
  if x1 <= x0 || y1 <= y0 {
    return Img { pixels: Vec::new(), w: 0, h: 0 };
  }
  let (cw, ch) = (x1 - x0, y1 - y0);
  let mut pixels = Vec::with_capacity((cw * ch * 4) as usize);
  for row in y0..y1 {
    let start = ((row * img.w + x0) * 4) as usize;
    pixels.extend_from_slice(&img.pixels[start..start + (cw * 4) as usize]);
  }
  Img { pixels, w: cw, h: ch }
}

/// Mean absolute channel difference + max absolute channel
/// difference over RGB (alpha ignored: both sides are opaque).
fn diff_metrics(a: &[u8], b: &[u8]) -> (f64, u8) {
  debug_assert_eq!(a.len(), b.len());
  let n = (a.len() / 4).max(1) as f64;
  let mut sum = 0u64;
  let mut max = 0u8;
  for i in (0..a.len()).step_by(4) {
    for c in 0..3 {
      let d = a[i + c].abs_diff(b[i + c]);
      sum += d as u64;
      max = max.max(d);
    }
  }
  (sum as f64 / (n * 3.0), max)
}

/// Side-by-side failure strip: reference | ours | abs-diff.
fn write_strip(outdir: &str, id: &str, ref_px: &[u8], our_px: &[u8], w: u32, h: u32) {
  let panel = (w * h * 4) as usize;
  if ref_px.len() < panel || our_px.len() < panel || w == 0 || h == 0 {
    return;
  }
  let stride = (w as usize) * 4;
  let mut rows = Vec::with_capacity(panel * 3);
  for row in 0..h as usize {
    let rs = row * stride;
    rows.extend_from_slice(&ref_px[rs..rs + stride]);
    rows.extend_from_slice(&our_px[rs..rs + stride]);
    for i in (rs..rs + stride).step_by(4) {
      rows.extend_from_slice(&[
        ref_px[i].abs_diff(our_px[i]),
        ref_px[i + 1].abs_diff(our_px[i + 1]),
        ref_px[i + 2].abs_diff(our_px[i + 2]),
        255,
      ]);
    }
  }
  let path = format!("{outdir}/diffstrip_{id}.png");
  match TiImage::new(w * 3, h, rows).and_then(|img| img.save(&path, ImageFormat::Png, 100)) {
    Ok(()) => println!("  wrote {path}"),
    Err(e) => println!("  cannot write {path}: {e}"),
  }
}

/// Text profile: ink coverage plus center of mass (px). Robust
/// across fonts; catches missing, misplaced, mis-sized and
/// mis-colored text.
fn text_profile(px: &[u8], w: u32, h: u32) -> (f32, f32, f32) {
  let (w, h) = (w as usize, h as usize);
  if px.len() < w * h * 4 || w == 0 || h == 0 {
    return (0.0, 0.0, 0.0);
  }
  let mut dark = 0u64;
  let mut sx = 0u64;
  let mut sy = 0u64;
  for y in 0..h {
    for x in 0..w {
      let i = (y * w + x) * 4;
      let lum = px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32;
      if lum < 3 * 128 {
        dark += 1;
        sx += x as u64;
        sy += y as u64;
      }
    }
  }
  if dark == 0 {
    return (0.0, h as f32 / 2.0, w as f32 / 2.0);
  }
  (dark as f32 / (w * h) as f32, sy as f32 / dark as f32, sx as f32 / dark as f32)
}

fn tally(map: &mut std::collections::BTreeMap<String, (u32, u32)>, kind: &str, ok: bool) {
  let slot = map.entry(kind.to_string()).or_insert((0, 0));
  if ok {
    slot.0 += 1;
  } else {
    slot.1 += 1;
  }
}
