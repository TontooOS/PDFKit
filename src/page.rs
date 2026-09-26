use crate::annot::Annotation;
use crate::error::Result;
use crate::graphics::{FontInfo, MapResources, PageItem, interpret, text_runs};
use crate::parser::PageFont;

/// One positioned text run on a page.
///
/// Coordinates are PDF points in default user space (origin at the
/// bottom-left of the page). The viewer converts them to top-left
/// logical pixels when drawing. `bold` is derived from the `/BaseFont`
/// name (it contains `Bold`); real font embedding comes later.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfTextRun {
  /// Decoded text of the run.
  pub text: String,
  /// X position of the run start in points (from the left).
  pub x: f32,
  /// Y position of the baseline in points (from the bottom).
  pub y: f32,
  /// Font size in points from the `Tf` operator.
  pub font_size: f32,
  /// True when the base font name contains `Bold`.
  pub bold: bool,
  /// Resource font name without slash, e.g. `F1`.
  pub font_name: String,
  /// Fill color from the graphics state as sRGB in `0.0..=1.0`.
  pub color_rgb: [f32; 3],
  /// Normalized text x-axis direction (for future underline/selection).
  pub dir_x: f32,
  /// Normalized text x-axis direction (for future underline/selection).
  pub dir_y: f32,
  /// Fill alpha from `ca` (`1.0` opaque).
  pub alpha: f32,
}

/// A fully interpreted page: size plus the vector/text item model.
///
/// The item list carries paths, text, clipping and state markers in
/// content order; `runs` is the text subset for convenience. Nothing
/// is rasterized, so the viewer and editor work on real coordinates.
#[derive(Debug, Clone)]
pub struct PdfPage {
  /// Zero-based page index in document order.
  pub number: usize,
  /// Page width in points.
  pub width: f32,
  /// Page height in points.
  pub height: f32,
  /// MediaBox origin (usually `0, 0`, kept for the view mapping).
  pub origin_x: f32,
  /// MediaBox origin (usually `0, 0`, kept for the view mapping).
  pub origin_y: f32,
  /// Text runs in content-stream order.
  pub runs: Vec<PdfTextRun>,
  /// All page items (text, paths, clipping, state markers).
  pub items: Vec<PageItem>,
  /// Page annotations (links, markup); filled by the document layer.
  pub annotations: Vec<Annotation>,
}

impl PdfPage {
  /// Plain text of the page (runs joined with spaces, lines by Y).
  pub fn text(&self) -> String {
    let mut runs = self.runs.clone();
    runs.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal).then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)));
    let mut out = String::new();
    let mut last_y = f32::NAN;
    for run in &runs {
      if !out.is_empty() {
        if last_y.is_nan() || (run.y - last_y).abs() > run.font_size * 0.5 {
          out.push('\n');
        } else {
          out.push(' ');
        }
      }
      out.push_str(&run.text);
      last_y = run.y;
    }
    out
  }

  /// Build a page from decoded content bytes and font resources.
  pub fn interpret(number: usize, media_box: [f32; 4], content: &[u8], fonts: &[PageFont]) -> Result<Self> {
    let width = (media_box[2] - media_box[0]).max(1.0);
    let height = (media_box[3] - media_box[1]).max(1.0);
    let provider = MapResources { fonts: fonts.iter().map(|f| (f.resource.clone(), FontInfo::simple(&f.resource, &f.base_font))).collect() };
    let items = interpret(content, provider)?;
    let runs = text_runs(&items);
    Ok(Self { number, width, height, origin_x: media_box[0], origin_y: media_box[1], runs, items, annotations: Vec::new() })
  }
}

/// Interpret content with a plain font list (used by tests).
pub fn interpret_content(content: &[u8], fonts: &[PageFont]) -> Result<Vec<PdfTextRun>> {
  let provider = MapResources { fonts: fonts.iter().map(|f| (f.resource.clone(), FontInfo::simple(&f.resource, &f.base_font))).collect() };
  Ok(text_runs(&interpret(content, provider)?))
}

/// Decode raw string bytes to text.
///
/// v0.1 assumes WinAnsiEncoding/Latin-1 for single-byte strings, which
/// covers the Helvetica/Times/Courier base-14 fonts used by most
/// simple PDFs. CID/UTF-16BE strings (`<FEFF...>`) are decoded when
/// they carry a BOM. Full CMap support is a later milestone.
pub fn decode_text(bytes: &[u8]) -> String {
  if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
    let units: Vec<u16> = bytes[2..].chunks(2).map(|c| ((c[0] as u16) << 8) | *c.get(1).unwrap_or(&0) as u16).collect();
    return String::from_utf16_lossy(&units);
  }
  bytes.iter().map(|&b| winansi_to_char(b)).collect()
}

fn winansi_to_char(b: u8) -> char {
  if b < 0x80 {
    b as char
  } else {
    match b {
      0x80 => '\u{20AC}',
      0x82 => '\u{201A}',
      0x83 => '\u{0192}',
      0x84 => '\u{201E}',
      0x85 => '\u{2026}',
      0x86 => '\u{2020}',
      0x87 => '\u{2021}',
      0x88 => '\u{02C6}',
      0x89 => '\u{2030}',
      0x8A => '\u{0160}',
      0x8B => '\u{2039}',
      0x8C => '\u{0152}',
      0x8E => '\u{017D}',
      0x91 => '\u{2018}',
      0x92 => '\u{2019}',
      0x93 => '\u{201C}',
      0x94 => '\u{201D}',
      0x95 => '\u{2022}',
      0x96 => '\u{2013}',
      0x97 => '\u{2014}',
      0x98 => '\u{02DC}',
      0x99 => '\u{2122}',
      0x9A => '\u{0161}',
      0x9B => '\u{203A}',
      0x9C => '\u{0153}',
      0x9E => '\u{017E}',
      0x9F => '\u{0178}',
      _ => b as char,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn fonts() -> Vec<PageFont> {
    vec![
      PageFont { resource: "F1".into(), base_font: "Helvetica".into() },
      PageFont { resource: "F2".into(), base_font: "Helvetica-Bold".into() },
    ]
  }

  #[test]
  fn shows_simple_tj_run() {
    let runs = interpret_content(b"BT /F1 12 Tf 72 720 Td (Hello) Tj ET", &fonts()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "Hello");
    assert_eq!(runs[0].x, 72.0);
    assert_eq!(runs[0].y, 720.0);
    assert!(!runs[0].bold);
  }

  #[test]
  fn handles_tm_and_tj_array_with_bold() {
    let runs = interpret_content(b"BT /F2 10 Tf 1 0 0 1 50 600 Tm [(Hel) -120 (lo)] TJ ET", &fonts()).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].text, "Hel");
    assert!(runs[0].bold);
    assert!(runs[1].x > runs[0].x);
  }

  #[test]
  fn handles_td_lines() {
    let runs = interpret_content(b"BT /F1 12 Tf 72 720 Td (One) Tj 0 -14 Td (Two) Tj ET", &fonts()).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].y, 706.0);
  }

  #[test]
  fn items_carry_paths() {
    let provider = MapResources::default();
    let items = interpret(b"0 0 10 10 re f BT /F1 12 Tf (t) Tj ET", provider).unwrap();
    assert_eq!(items.len(), 2);
    assert!(matches!(items[0], PageItem::Path(_)));
    assert!(matches!(items[1], PageItem::Text(_)));
  }
}
