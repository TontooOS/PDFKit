use crate::graphics::Rgb;
use crate::objects::PdfValue;

/// A decoded image in straight (unpremultiplied) RGBA8.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedImage {
  /// Pixel width.
  pub width: u32,
  /// Pixel height.
  pub height: u32,
  /// Row-major RGBA bytes, top row first.
  pub rgba: Vec<u8>,
  /// `/Interpolate` hint from the dict.
  pub interpolate: bool,
}

/// Decode an image XObject body (already filtered samples, except
/// DCT which arrives as raw JPEG) into RGBA.
///
/// `dict` is the image dictionary, `samples` the decoded sample
/// bytes. `map` converts normalized component values to sRGB for
/// non-device spaces. Returns `None` for unsupported configurations
/// (JPX, CCITT-backed, bad dimensions) so callers skip the image
/// instead of failing the page.
pub fn decode_samples(
  dict: &PdfValue,
  samples: &[u8],
  map: &dyn Fn(&[f32]) -> Option<Rgb>,
) -> Option<DecodedImage> {
  let width = dict.get("Width").and_then(|v| v.as_number()).unwrap_or(0.0) as u32;
  let height = dict.get("Height").and_then(|v| v.as_number()).unwrap_or(0.0) as u32;
  if width == 0 || height == 0 || width > 16384 || height > 16384 {
    return None;
  }
  let interpolate = dict.get("Interpolate").and_then(|v| v.as_number()).unwrap_or(0.0) != 0.0;
  // Stencil masks paint with the current color (handled by caller).
  if dict.get("ImageMask").and_then(|v| v.as_number()).is_some_and(|v| v != 0.0) {
    return None;
  }
  let bpc = dict.get("BitsPerComponent").and_then(|v| v.as_number()).unwrap_or(1.0) as usize;
  if ![1, 2, 4, 8, 16].contains(&bpc) {
    return None;
  }
  let space = dict.get("ColorSpace")?;
  let ncomp = component_count(space)?;
  let stride = (width as usize * ncomp * bpc + 7) / 8;
  if samples.len() < stride * height as usize {
    return None;
  }
  let decode = decode_array(dict, ncomp);
  let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
  for row in 0..height as usize {
    let line = &samples[row * stride..];
    for col in 0..width as usize {
      let mut comps = Vec::with_capacity(ncomp);
      for c in 0..ncomp {
        let raw = sample_at(line, col * ncomp + c, bpc, width as usize * ncomp);
        let v = raw / max_sample(bpc);
        let (lo, hi) = (decode[2 * c], decode[2 * c + 1]);
        comps.push(lo + v * (hi - lo));
      }
      let rgb = map_device(space, &comps).or_else(|| map(&comps))?;
      rgba.push((rgb.r.clamp(0.0, 1.0) * 255.0) as u8);
      rgba.push((rgb.g.clamp(0.0, 1.0) * 255.0) as u8);
      rgba.push((rgb.b.clamp(0.0, 1.0) * 255.0) as u8);
      rgba.push(255);
    }
  }
  Some(DecodedImage { width, height, rgba, interpolate })
}

/// Decode a stencil mask (1 bpc) into an alpha plane.
/// A stencil paints where the decoded sample is 0: with the default
/// `/Decode [0 1]` the 0-bits paint (the mask transfers the image's
/// black bits, matching poppler and Firefox), while an explicit
/// `/Decode [1 0]` inverts this so the 1-bits paint.
pub fn decode_mask_alpha(dict: &PdfValue, samples: &[u8]) -> Option<Vec<u8>> {
  let width = dict.get("Width").and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
  let height = dict.get("Height").and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
  if width == 0 || height == 0 || width > 16384 || height > 16384 {
    return None;
  }
  let stride = (width + 7) / 8;
  if samples.len() < stride * height {
    return None;
  }
  let decode = decode_array(dict, 1);
  let mut alpha = Vec::with_capacity(width * height);
  for row in 0..height {
    for col in 0..width {
      let byte = samples[row * stride + col / 8];
      let bit = (byte >> (7 - (col % 8))) & 1;
      let v = decode[0] + bit as f32 * (decode[1] - decode[0]);
      alpha.push(if v < 0.5 { 255 } else { 0 });
    }
  }
  Some(alpha)
}

/// Decode a soft mask (grayscale) into an alpha plane.
pub fn decode_smask_alpha(
  dict: &PdfValue,
  samples: &[u8],
  map: &dyn Fn(&[f32]) -> Option<Rgb>,
) -> Option<Vec<u8>> {
  let width = dict.get("Width").and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
  let height = dict.get("Height").and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
  if width == 0 || height == 0 || width > 16384 || height > 16384 {
    return None;
  }
  let bpc = dict.get("BitsPerComponent").and_then(|v| v.as_number()).unwrap_or(8.0) as usize;
  if ![1, 2, 4, 8, 16].contains(&bpc) {
    return None;
  }
  // Soft masks use their own color space (usually DeviceGray).
  let space = dict.get("ColorSpace");
  let ncomp = space.as_ref().and_then(|s| component_count(s)).unwrap_or(1).max(1);
  let stride = (width * ncomp * bpc + 7) / 8;
  if samples.len() < stride * height {
    return None;
  }
  let decode = decode_array(dict, ncomp);
  let mut alpha = Vec::with_capacity(width * height);
  for row in 0..height {
    let line = &samples[row * stride..];
    for col in 0..width {
      let mut lum = 0.0;
      for c in 0..ncomp {
        let raw = sample_at(line, col * ncomp + c, bpc, width * ncomp);
        let v = raw / max_sample(bpc);
        let (lo, hi) = (decode[2 * c], decode[2 * c + 1]);
        lum += lo + v * (hi - lo);
      }
      lum /= ncomp as f32;
      // Map through the space when it is not plain gray.
      let a = match space {
        Some(s) if component_count(s).is_some_and(|n| n > 1) => {
          let comps: Vec<f32> = (0..ncomp)
            .map(|c| {
              let raw = sample_at(line, col * ncomp + c, bpc, width * ncomp);
              raw / max_sample(bpc)
            })
            .collect();
          map(&comps).map(|rgb| (rgb.r + rgb.g + rgb.b) / 3.0).unwrap_or(lum)
        }
        _ => lum,
      };
      alpha.push((a.clamp(0.0, 1.0) * 255.0) as u8);
    }
  }
  Some(alpha)
}

/// Decode JPEG bytes (DCTDecode) to RGBA through CoreImage.
pub fn decode_jpeg(data: &[u8]) -> Option<DecodedImage> {
  let img = coreimage::io::from_bytes_with_format(data, coreimage::io::ImageFormat::Jpeg).ok()?;
  let rgba = img.into_rgba();
  let (width, height) = (rgba.width(), rgba.height());
  if width == 0 || height == 0 {
    return None;
  }
  Some(DecodedImage { width, height, rgba: rgba.into_raw(), interpolate: true })
}

fn component_count(space: &PdfValue) -> Option<usize> {
  match space {
    PdfValue::Name(n) => match n.as_str() {
      "DeviceGray" | "G" => Some(1),
      "DeviceRGB" | "RGB" => Some(3),
      "DeviceCMYK" | "CMYK" => Some(4),
      _ => None,
    },
    PdfValue::Array(items) => match items.first()?.as_name()? {
      "CalGray" => Some(1),
      "CalRGB" | "Lab" => Some(3),
      "ICCBased" => None,
      "Indexed" | "I" => Some(1),
      "Separation" => Some(1),
      "DeviceN" => items.get(1).and_then(|v| v.as_array()).map(|a| a.len().max(1)),
      "Pattern" => None,
      _ => None,
    },
    _ => None,
  }
}

fn decode_array(dict: &PdfValue, ncomp: usize) -> Vec<f32> {
  let mut out = Vec::with_capacity(2 * ncomp);
  if let Some(items) = dict.get("Decode").and_then(|v| v.as_array()) {
    for item in items.iter().take(2 * ncomp) {
      out.push(item.as_number().unwrap_or(0.0) as f32);
    }
  }
  while out.len() < 2 * ncomp {
    let pair = out.len() % 2 == 0;
    out.push(if pair { 0.0 } else { 1.0 });
  }
  out
}

fn max_sample(bpc: usize) -> f32 {
  ((1u32 << bpc) - 1) as f32
}

fn sample_at(line: &[u8], index: usize, bpc: usize, _total: usize) -> f32 {
  if bpc >= 8 {
    let step = bpc / 8;
    let at = index * step;
    let mut v = 0u32;
    for b in line.iter().skip(at).take(step) {
      v = (v << 8) | u32::from(*b);
    }
    return v as f32;
  }
  let bit = index * bpc;
  let mut v = 0u32;
  for k in 0..bpc {
    let pos = bit + k;
    let byte = *line.get(pos / 8).unwrap_or(&0);
    v = (v << 1) | ((byte >> (7 - (pos % 8))) & 1) as u32;
  }
  v as f32
}

fn map_device(space: &PdfValue, comps: &[f32]) -> Option<Rgb> {
  match space {
    PdfValue::Name(n) => match n.as_str() {
      "DeviceGray" | "G" => {
        let g = comps.first().copied().unwrap_or(0.0);
        Some(Rgb { r: g, g, b: g })
      }
      "DeviceRGB" | "RGB" => Some(Rgb {
        r: comps.first().copied().unwrap_or(0.0),
        g: comps.get(1).copied().unwrap_or(0.0),
        b: comps.get(2).copied().unwrap_or(0.0),
      }),
      "DeviceCMYK" | "CMYK" => Some(crate::graphics::cmyk_to_rgb(
        comps.first().copied().unwrap_or(0.0),
        comps.get(1).copied().unwrap_or(0.0),
        comps.get(2).copied().unwrap_or(0.0),
        comps.get(3).copied().unwrap_or(1.0),
      )),
      _ => None,
    },
    _ => None,
  }
}

/// Apply an alpha plane to RGBA bytes in place.
pub fn apply_alpha(rgba: &mut [u8], alpha: &[u8]) {
  for (px, a) in rgba.chunks_exact_mut(4).zip(alpha.iter()) {
    px[3] = ((px[3] as u16 * *a as u16) / 255) as u8;
  }
}

/// Multiply a constant alpha into RGBA bytes in place.
pub fn apply_constant_alpha(rgba: &mut [u8], alpha: f32) {
  let a = (alpha.clamp(0.0, 1.0) * 255.0) as u16;
  for px in rgba.chunks_exact_mut(4) {
    px[3] = ((px[3] as u16 * a) / 255) as u8;
  }
}

/// Build a test image dict value.
#[cfg(test)]
pub fn test_dict(pairs: Vec<(&str, PdfValue)>) -> PdfValue {
  PdfValue::Dict(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn gray_map(comps: &[f32]) -> Option<Rgb> {
    let g = comps.first().copied().unwrap_or(0.0);
    Some(Rgb { r: g, g, b: g })
  }

  #[test]
  fn decodes_gray_image() {
    let dict = test_dict(vec![
      ("Width".into(), PdfValue::Number(2.0)),
      ("Height".into(), PdfValue::Number(2.0)),
      ("BitsPerComponent".into(), PdfValue::Number(8.0)),
      ("ColorSpace".into(), PdfValue::Name("DeviceGray".into())),
    ]);
    let img = decode_samples(&dict, &[0, 255, 128, 64], &gray_map).unwrap();
    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(&img.rgba[0..4], &[0, 0, 0, 255]);
    assert_eq!(&img.rgba[4..8], &[255, 255, 255, 255]);
  }

  #[test]
  fn decodes_one_bit_rgb() {
    let dict = test_dict(vec![
      ("Width".into(), PdfValue::Number(2.0)),
      ("Height".into(), PdfValue::Number(1.0)),
      ("BitsPerComponent".into(), PdfValue::Number(1.0)),
      ("ColorSpace".into(), PdfValue::Name("DeviceRGB".into())),
    ]);
    // Pixels: white (111) then black (000), packed MSB-first.
    let img = decode_samples(&dict, &[0b11100000], &gray_map).unwrap();
    assert_eq!(&img.rgba[0..4], &[255, 255, 255, 255]);
    assert_eq!(&img.rgba[4..8], &[0, 0, 0, 255]);
  }

  #[test]
  fn decode_array_inverts() {
    let dict = test_dict(vec![
      ("Width".into(), PdfValue::Number(1.0)),
      ("Height".into(), PdfValue::Number(1.0)),
      ("BitsPerComponent".into(), PdfValue::Number(8.0)),
      ("ColorSpace".into(), PdfValue::Name("DeviceGray".into())),
      (
        "Decode".into(),
        PdfValue::Array(vec![PdfValue::Number(1.0), PdfValue::Number(0.0)]),
      ),
    ]);
    let img = decode_samples(&dict, &[0], &gray_map).unwrap();
    assert_eq!(&img.rgba[0..3], &[255, 255, 255]);
  }

  #[test]
  fn mask_polarity() {
    let dict = test_dict(vec![
      ("Width".into(), PdfValue::Number(8.0)),
      ("Height".into(), PdfValue::Number(1.0)),
    ]);
    // Default Decode [0 1]: 0-bits paint (black bits transfer).
    assert_eq!(
      decode_mask_alpha(&dict, &[0b10100000]).unwrap(),
      vec![0, 255, 0, 255, 255, 255, 255, 255]
    );
  }

  #[test]
  fn mask_explicit_decode_inverts() {
    let dict = test_dict(vec![
      ("Width".into(), PdfValue::Number(8.0)),
      ("Height".into(), PdfValue::Number(1.0)),
      (
        "Decode".into(),
        PdfValue::Array(vec![PdfValue::Number(1.0), PdfValue::Number(0.0)]),
      ),
    ]);
    // Explicit Decode [1 0]: 1-bits paint.
    assert_eq!(
      decode_mask_alpha(&dict, &[0b10100000]).unwrap(),
      vec![255, 0, 255, 0, 0, 0, 0, 0]
    );
  }

  #[test]
  fn mask_e080_stencil_paints_right_column() {
    // Coverage element E080: 2x2 stencil rows 0x80 (col0 = 1,
    // col1 = 0). The reference (poppler/Firefox) paints the right
    // column, so the alpha plane must be [0, 255, 0, 255].
    let dict = test_dict(vec![
      ("Width".into(), PdfValue::Number(2.0)),
      ("Height".into(), PdfValue::Number(2.0)),
    ]);
    assert_eq!(decode_mask_alpha(&dict, &[0x80, 0x80]).unwrap(), vec![0, 255, 0, 255]);
  }

  #[test]
  fn rejects_bad_dims() {
    let dict = test_dict(vec![
      ("Width".into(), PdfValue::Number(0.0)),
      ("Height".into(), PdfValue::Number(4.0)),
      ("ColorSpace".into(), PdfValue::Name("DeviceGray".into())),
    ]);
    assert!(decode_samples(&dict, &[], &gray_map).is_none());
  }

  #[test]
  fn jpeg_garbage_skips() {
    assert!(decode_jpeg(b"not a jpeg").is_none());
  }
}
