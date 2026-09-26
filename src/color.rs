use crate::error::{PdfError, Result};
use crate::graphics::Rgb;
use crate::objects::PdfValue;

/// A PDF function (ISO 32000 7.10) mapping `m` inputs to `n` outputs.
/// Type 4 (PostScript calculator) is not evaluated; it parses to
/// `Unsupported` so callers can fall back gracefully.
#[derive(Debug, Clone)]
pub enum Function {
  Sampled(Sampled),
  Exponential { c0: Vec<f32>, c1: Vec<f32>, n: f32, domain: Vec<f32>, range: Option<Vec<f32>> },
  Stitching { funcs: Vec<Function>, bounds: Vec<f32>, encode: Vec<f32>, domain: Vec<f32>, range: Option<Vec<f32>> },
  Unsupported,
}

/// Type 0 sampled function with multilinear interpolation (`m <= 4`).
#[derive(Debug, Clone)]
pub struct Sampled {
  pub size: Vec<usize>,
  pub bps: usize,
  pub encode: Vec<f32>,
  pub decode: Vec<f32>,
  pub domain: Vec<f32>,
  pub range: Option<Vec<f32>>,
  pub table: Vec<f32>,
}

/// Parse a function from a resolved value (dict or array of functions).
/// References must be resolved by the caller.
pub fn parse_function(value: &PdfValue) -> Result<Function> {
  match value {
    PdfValue::Array(items) => {
      // Array of functions: treat as single-element stitching when
      // more than one entry exists.
      if items.len() == 1 {
        return parse_function(&items[0]);
      }
      let mut funcs = Vec::new();
      for item in items {
        funcs.push(parse_function(item)?);
      }
      Ok(Function::Stitching { funcs, bounds: vec![], encode: vec![], domain: vec![0.0, 1.0], range: None })
    }
    PdfValue::Dict(_) => {
      let kind = value.get("FunctionType").and_then(|v| v.as_number()).unwrap_or(-1.0) as i64;
      match kind {
        0 => Err(PdfError::InvalidObject("sampled function needs stream data".into())),
        2 => {
          let c0 = num_array(value, "C0", &[0.0]);
          let c1 = num_array(value, "C1", &[1.0]);
          let n = value.get("N").and_then(|v| v.as_number()).unwrap_or(1.0) as f32;
          let domain = num_array(value, "Domain", &[0.0, 1.0]);
          let range = opt_num_array(value, "Range");
          Ok(Function::Exponential { c0, c1, n, domain, range })
        }
        3 => {
          let sub = value.get("Functions").and_then(|v| v.as_array()).map(|a| a.to_vec()).unwrap_or_default();
          let mut funcs = Vec::new();
          for item in &sub {
            funcs.push(parse_function(item)?);
          }
          if funcs.is_empty() {
            return Err(PdfError::InvalidObject("stitching without functions".into()));
          }
          Ok(Function::Stitching {
            bounds: num_array(value, "Bounds", &[]),
            encode: num_array(value, "Encode", &[]),
            domain: num_array(value, "Domain", &[0.0, 1.0]),
            range: opt_num_array(value, "Range"),
            funcs,
          })
        }
        4 => Ok(Function::Unsupported),
        _ => Err(PdfError::InvalidObject("bad FunctionType".into())),
      }
    }
    _ => Err(PdfError::InvalidObject("function must be a dict or array".into())),
  }
}

/// Parse a sampled function from its dict plus decoded stream bytes.
pub fn parse_sampled(dict: &PdfValue, data: &[u8]) -> Result<Function> {
  let size: Vec<usize> = num_array(dict, "Size", &[]).iter().map(|v| *v as usize).collect();
  if size.is_empty() || size.len() > 4 || size.iter().any(|s| *s == 0) {
    return Err(PdfError::InvalidObject("bad sampled Size".into()));
  }
  let bps = dict.get("BitsPerSample").and_then(|v| v.as_number()).unwrap_or(1.0) as usize;
  if ![1, 2, 4, 8, 12, 16, 24, 32].contains(&bps) {
    return Err(PdfError::InvalidObject("bad BitsPerSample".into()));
  }
  let m = size.len();
  let n_out = dict
    .get("Range")
    .and_then(|v| v.as_array())
    .map(|a| a.len() / 2)
    .unwrap_or(1)
    .max(1);
  let total: usize = size.iter().product::<usize>() * n_out;
  let table = unpack_samples(data, bps, total)?;
  let mut encode = num_array(dict, "Encode", &[]);
  if encode.is_empty() {
    for s in &size {
      encode.extend_from_slice(&[0.0, (*s as f32) - 1.0]);
    }
  }
  let domain = num_array(dict, "Domain", &vec![0.0, 1.0]);
  let domain = if domain.len() == 2 * m { domain } else { vec![0.0, 1.0] };
  let decode = num_array(dict, "Decode", &[]);
  let decode = if decode.len() == 2 * n_out { decode } else { (0..n_out).flat_map(|_| [0.0, 1.0]).collect() };
  Ok(Function::Sampled(Sampled {
    size,
    bps,
    encode,
    decode,
    domain,
    range: opt_num_array(dict, "Range"),
    table,
  }))
}

fn unpack_samples(data: &[u8], bps: usize, total: usize) -> Result<Vec<f32>> {
  let max = ((1u64 << bps) - 1) as f32;
  let mut out = Vec::with_capacity(total);
  if bps >= 8 {
    let step = bps / 8;
    for chunk in data.chunks(step).take(total) {
      let mut v = 0u64;
      for b in chunk {
        v = (v << 8) | u64::from(*b);
      }
      out.push(v as f32 / max);
    }
  } else {
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut pos = 0;
    while out.len() < total {
      while bits < bps as u32 {
        let b = *data.get(pos).ok_or_else(|| PdfError::StreamDecode("truncated samples".into()))?;
        pos += 1;
        acc = (acc << 8) | b as u32;
        bits += 8;
      }
      bits -= bps as u32;
      out.push(((acc >> bits) & ((1 << bps) - 1)) as f32 / max);
      acc &= (1 << bits) - 1;
    }
  }
  while out.len() < total {
    out.push(0.0);
  }
  Ok(out)
}

fn num_array(dict: &PdfValue, key: &str, fallback: &[f32]) -> Vec<f32> {
  match dict.get(key).and_then(|v| v.as_array()) {
    Some(items) => items.iter().filter_map(|v| v.as_number().map(|n| n as f32)).collect(),
    None => fallback.to_vec(),
  }
}

fn opt_num_array(dict: &PdfValue, key: &str) -> Option<Vec<f32>> {
  dict.get(key).and_then(|v| v.as_array()).map(|items| {
    items.iter().filter_map(|v| v.as_number().map(|n| n as f32)).collect()
  })
}

fn clip_to_domain(x: &[f32], domain: &[f32]) -> Vec<f32> {
  x.iter()
    .enumerate()
    .map(|(i, v)| {
      let (lo, hi) = (domain.get(2 * i).copied().unwrap_or(0.0), domain.get(2 * i + 1).copied().unwrap_or(1.0));
      v.clamp(lo.min(hi), lo.max(hi))
    })
    .collect()
}

fn clip_to_range(y: Vec<f32>, range: &Option<Vec<f32>>) -> Vec<f32> {
  match range {
    None => y,
    Some(r) => y
      .into_iter()
      .enumerate()
      .map(|(i, v)| {
        let (lo, hi) = (r.get(2 * i).copied().unwrap_or(0.0), r.get(2 * i + 1).copied().unwrap_or(1.0));
        v.clamp(lo.min(hi), lo.max(hi))
      })
      .collect(),
  }
}

impl Function {
  /// Evaluate the function. Returns an empty vec for `Unsupported`.
  pub fn eval(&self, x: &[f32]) -> Vec<f32> {
    match self {
      Self::Unsupported => vec![],
      Self::Exponential { c0, c1, n, domain, range } => {
        let xc = clip_to_domain(x, domain);
        let t = xc.first().copied().unwrap_or(0.0);
        let out: Vec<f32> = c0
          .iter()
          .zip(c1.iter().chain(std::iter::repeat(&1.0)))
          .map(|(a, b)| a + t.powf(*n) * (b - a))
          .collect();
        clip_to_range(out, range)
      }
      Self::Stitching { funcs, bounds, encode, domain, range } => {
        let xc = clip_to_domain(x, domain);
        let t = xc.first().copied().unwrap_or(0.0);
        let mut index = 0;
        for (i, bound) in bounds.iter().enumerate() {
          if t >= *bound {
            index = i + 1;
          }
        }
        index = index.min(funcs.len().saturating_sub(1));
        let (lo, hi) = if bounds.is_empty() {
          (0.0, 1.0)
        } else {
          let lo = if index == 0 { domain.first().copied().unwrap_or(0.0) } else { bounds[index - 1] };
          let hi = bounds.get(index).copied().unwrap_or_else(|| domain.get(1).copied().unwrap_or(1.0));
          (lo, hi)
        };
        let (e0, e1) = (encode.get(2 * index).copied().unwrap_or(0.0), encode.get(2 * index + 1).copied().unwrap_or(1.0));
        let mapped = if (hi - lo).abs() < 1e-6 { e0 } else { e0 + (t - lo) / (hi - lo) * (e1 - e0) };
        let out = funcs[index].eval(&[mapped]);
        clip_to_range(out, range)
      }
      Self::Sampled(s) => {
        let m = s.size.len();
        let xc = clip_to_domain(x, &s.domain);
        // Map input to sample coordinates.
        let mut coords = Vec::with_capacity(m);
        for i in 0..m {
          let (d0, d1) = (s.domain.get(2 * i).copied().unwrap_or(0.0), s.domain.get(2 * i + 1).copied().unwrap_or(1.0));
          let (e0, e1) = (s.encode.get(2 * i).copied().unwrap_or(0.0), s.encode.get(2 * i + 1).copied().unwrap_or(1.0));
          let t = if (d1 - d0).abs() < 1e-6 { 0.0 } else { ((xc.get(i).copied().unwrap_or(0.0) - d0) / (d1 - d0)).clamp(0.0, 1.0) };
          coords.push(e0 + t * (e1 - e0));
        }
        let n_out = s.decode.len() / 2;
        let mut out = vec![0.0; n_out];
        // Multilinear interpolation over the 2^m corners.
        for corner in 0..(1 << m) {
          let mut weight = 1.0;
          let mut addr = 0usize;
          let mut stride = 1usize;
          for i in 0..m {
            let size = s.size[i];
            let c = coords[i].clamp(0.0, (size as f32) - 1.0);
            let lo = c.floor() as usize;
            let frac = c - lo as f32;
            let bit = (corner >> i) & 1;
            let idx = (lo + bit).min(size - 1);
            weight *= if bit == 1 { frac } else { 1.0 - frac };
            addr += idx * stride;
            stride *= size;
          }
          for o in 0..n_out {
            let sample = s.table.get(addr * n_out + o).copied().unwrap_or(0.0);
            let (lo, hi) = (s.decode.get(2 * o).copied().unwrap_or(0.0), s.decode.get(2 * o + 1).copied().unwrap_or(1.0));
            out[o] += weight * (lo + sample * (hi - lo));
          }
        }
        clip_to_range(out, &s.range)
      }
    }
  }
}

/// CalGray approximation: gamma on white-scaled input.
pub fn calgray_to_rgb(gray: f32, gamma: f32) -> Rgb {
  let g = gray.clamp(0.0, 1.0).powf(gamma.max(0.01));
  Rgb { r: g, g, b: g }
}

/// CalRGB approximation: gamma per component plus matrix to XYZ,
/// then XYZ (D65 default white) to sRGB.
pub fn calrgb_to_rgb(rgb: [f32; 3], gamma: [f32; 3], matrix: [f32; 9]) -> Rgb {
  let lin = [
    rgb[0].clamp(0.0, 1.0).powf(gamma[0].max(0.01)),
    rgb[1].clamp(0.0, 1.0).powf(gamma[1].max(0.01)),
    rgb[2].clamp(0.0, 1.0).powf(gamma[2].max(0.01)),
  ];
  let xyz = [
    matrix[0] * lin[0] + matrix[1] * lin[1] + matrix[2] * lin[2],
    matrix[3] * lin[0] + matrix[4] * lin[1] + matrix[5] * lin[2],
    matrix[6] * lin[0] + matrix[7] * lin[1] + matrix[8] * lin[2],
  ];
  xyz_d65_to_srgb(xyz).clamp()
}

/// Standard XYZ (D65) to linear sRGB matrix plus gamma encoding.
fn xyz_d65_to_srgb(xyz: [f32; 3]) -> Rgb {
  let r = 3.2404542 * xyz[0] - 1.5371385 * xyz[1] - 0.4985314 * xyz[2];
  let g = -0.9692660 * xyz[0] + 1.8760108 * xyz[1] + 0.0415560 * xyz[2];
  let b = 0.0556434 * xyz[0] - 0.2040259 * xyz[1] + 1.0572252 * xyz[2];
  Rgb { r: gamma_encode(r), g: gamma_encode(g), b: gamma_encode(b) }
}

/// XYZ (D50) to linear sRGB with Bradford adaptation folded in.
fn xyz_d50_to_srgb(xyz: [f32; 3]) -> Rgb {
  let r = 3.1338561 * xyz[0] - 1.6168667 * xyz[1] - 0.4906146 * xyz[2];
  let g = -0.9787684 * xyz[0] + 1.9161415 * xyz[1] + 0.0334540 * xyz[2];
  let b = 0.0719453 * xyz[0] - 0.2289914 * xyz[1] + 1.4052427 * xyz[2];
  Rgb { r: gamma_encode(r), g: gamma_encode(g), b: gamma_encode(b) }
}

fn gamma_encode(v: f32) -> f32 {
  if v <= 0.0031308 {
    12.92 * v
  } else {
    1.055 * v.powf(1.0 / 2.4) - 0.055
  }
}

/// CIELab (D50 white point) to sRGB approximation.
pub fn lab_to_rgb(lab: [f32; 3]) -> Rgb {
  let (l, a, b) = (lab[0], lab[1], lab[2]);
  let fy = (l + 16.0) / 116.0;
  let fx = a / 500.0 + fy;
  let fz = fy - b / 200.0;
  let pivot = |t: f32| {
    let t3 = t * t * t;
    if t3 > 0.008856 {
      t3
    } else {
      (t - 16.0 / 116.0) / 7.787
    }
  };
  // D50 white point.
  let xyz = [pivot(fx) * 0.9642, pivot(fy), pivot(fz) * 0.8251];
  xyz_d50_to_srgb(xyz).clamp()
}

/// Map an Indexed color space index through the lookup table.
/// Returns base-space components (caller maps them further).
pub fn indexed_lookup(table: &[u8], hival: usize, index: usize, comps_per_entry: usize) -> Vec<f32> {
  let idx = index.min(hival);
  let base = idx * comps_per_entry;
  (0..comps_per_entry).map(|i| table.get(base + i).copied().unwrap_or(0) as f32 / 255.0).collect()
}

/// A resolved shading ready for the view (axial/radial only;
/// meshes and function-based shadings stay `Unsupported`).
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedShading {
  Axial { coords: [f32; 4], stops: Vec<(f32, Rgb)>, extend: [bool; 2] },
  Radial { coords: [f32; 6], stops: Vec<(f32, Rgb)>, extend: [bool; 2] },
  Unsupported,
}

/// A resolved pattern reference.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedPattern {
  /// Tiling patterns render in a later milestone.
  Tiling,
  Shading(ResolvedShading),
}

/// Sample a shading function into gradient stops.
/// `map` converts function output components to sRGB.
pub fn shading_stops(func: &Function, map: &dyn Fn(&[f32]) -> Rgb, steps: usize) -> Vec<(f32, Rgb)> {
  let steps = steps.max(2);
  (0..steps)
    .map(|i| {
      let t = i as f32 / (steps - 1) as f32;
      (t, map(&func.eval(&[t])))
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn exponential_interpolates() {
    let f = Function::Exponential { c0: vec![0.0], c1: vec![1.0], n: 1.0, domain: vec![0.0, 1.0], range: None };
    assert_eq!(f.eval(&[0.25]), vec![0.25]);
  }

  #[test]
  fn exponential_gamma_two() {
    let f = Function::Exponential { c0: vec![0.0], c1: vec![1.0], n: 2.0, domain: vec![0.0, 1.0], range: None };
    assert!((f.eval(&[0.5])[0] - 0.25).abs() < 1e-6);
  }

  #[test]
  fn stitching_selects_branch() {
    let lo = Function::Exponential { c0: vec![0.0], c1: vec![0.5], n: 1.0, domain: vec![0.0, 1.0], range: None };
    let hi = Function::Exponential { c0: vec![0.5], c1: vec![1.0], n: 1.0, domain: vec![0.0, 1.0], range: None };
    let f = Function::Stitching {
      funcs: vec![lo, hi],
      bounds: vec![0.5],
      encode: vec![0.0, 1.0, 0.0, 1.0],
      domain: vec![0.0, 1.0],
      range: None,
    };
    assert!((f.eval(&[0.25])[0] - 0.25).abs() < 1e-6);
    assert!((f.eval(&[0.75])[0] - 0.75).abs() < 1e-6);
  }

  #[test]
  fn sampled_identity() {
    // 1-in/1-out identity over 4 samples.
    let dict = PdfValue::Dict(vec![
      ("Size".into(), PdfValue::Array(vec![PdfValue::Number(4.0)])),
      ("BitsPerSample".into(), PdfValue::Number(8.0)),
    ]);
    let data = [0u8, 85, 170, 255];
    let f = parse_sampled(&dict, &data).unwrap();
    assert!((f.eval(&[0.0])[0] - 0.0).abs() < 1e-6);
    assert!((f.eval(&[1.0])[0] - 1.0).abs() < 1e-6);
    let mid = f.eval(&[0.5])[0];
    assert!((mid - 0.5).abs() < 0.02, "mid={mid}");
  }

  #[test]
  fn lab_mid_gray() {
    let rgb = lab_to_rgb([50.0, 0.0, 0.0]);
    assert!((rgb.r - rgb.g).abs() < 0.02 && (rgb.g - rgb.b).abs() < 0.02);
    assert!(rgb.r > 0.1 && rgb.r < 0.5, "r={}", rgb.r);
  }

  #[test]
  fn indexed_maps_entries() {
    let table = [255u8, 0, 0, 0, 255, 0];
    assert_eq!(indexed_lookup(&table, 1, 1, 3), vec![0.0, 1.0, 0.0]);
    assert_eq!(indexed_lookup(&table, 1, 9, 3), vec![0.0, 1.0, 0.0]);
  }
}
