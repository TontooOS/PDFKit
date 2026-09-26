use std::collections::HashMap;

use crate::color::{Function, ResolvedPattern, ResolvedShading, calgray_to_rgb, calrgb_to_rgb, indexed_lookup, lab_to_rgb, parse_function, parse_sampled, shading_stops};
use crate::error::{PdfError, Result};
use crate::graphics::{ExtGState, FontInfo, ResourceProvider, Rgb, interpret, text_runs};
use crate::objects::PdfValue;
use crate::page::PdfPage;
use crate::parser::FileParser;

/// A loaded PDF document: parsed structure plus interpreted pages.
///
/// The document owns its bytes so pages can be re-interpreted on
/// demand (needed later for the editor without re-reading the file).
/// Pages parse eagerly at load; large documents stream later.
#[derive(Debug)]
pub struct PdfDocument {
  pages: Vec<PdfPage>,
}

impl PdfDocument {
  /// Parse a document from memory.
  pub fn load_bytes(data: Vec<u8>) -> Result<Self> {
    let parser = FileParser::new(data)?;
    Self::from_parser(&parser)
  }

  /// Parse a document from a file path.
  pub fn load_file(path: &str) -> Result<Self> {
    let data = std::fs::read(path).map_err(|e| PdfError::InvalidObject(e.to_string()))?;
    Self::load_bytes(data)
  }

  fn from_parser(parser: &FileParser) -> Result<Self> {
    let parsed = parser.pages()?;
    let mut pages = Vec::with_capacity(parsed.len());
    for item in &parsed {
      let provider = DocProvider {
        parser,
        resources: item.resources.clone(),
        fonts: item.fonts.iter().map(|f| (f.resource.clone(), f.base_font.clone())).collect(),
      };
      let items = interpret(&item.content, provider)?;
      let runs = text_runs(&items);
      let width = (item.media_box[2] - item.media_box[0]).max(1.0);
      let height = (item.media_box[3] - item.media_box[1]).max(1.0);
      pages.push(PdfPage {
        number: item.index,
        width,
        height,
        origin_x: item.media_box[0],
        origin_y: item.media_box[1],
        runs,
        items,
      });
    }
    if pages.is_empty() {
      return Err(PdfError::NoPages);
    }
    Ok(Self { pages })
  }

  /// Number of pages in the document.
  pub fn page_count(&self) -> usize {
    self.pages.len()
  }

  /// Access page `index` (zero-based). Returns `PageOutOfRange` when
  /// the index is invalid.
  pub fn page(&self, index: usize) -> Result<&PdfPage> {
    self.pages.get(index).ok_or(PdfError::PageOutOfRange(index))
  }

  /// True when the document has no text on any page.
  pub fn is_empty_text(&self) -> bool {
    self.pages.iter().all(|p| p.runs.is_empty())
  }
}

/// Document-backed resources for one page: fonts plus reference
/// resolution for ExtGState, color spaces, shadings and patterns.
struct DocProvider<'a> {
  parser: &'a FileParser,
  resources: PdfValue,
  fonts: HashMap<String, String>,
}

impl<'a> ResourceProvider for DocProvider<'a> {
  fn font(&self, name: &str) -> Option<FontInfo> {
    self.fonts.get(name).map(|base_font| FontInfo { resource: name.into(), base_font: base_font.clone() })
  }

  fn extgstate(&self, name: &str) -> Option<ExtGState> {
    let dict = self.parser.resource_entry(&self.resources, "ExtGState", name)?;
    let mut gs = ExtGState::default();
    gs.lw = dict.get("LW").and_then(|v| v.as_number()).map(|v| v as f32);
    gs.lc = dict.get("LC").and_then(|v| v.as_number()).map(|v| (v as u8).min(2));
    gs.join = dict.get("LJ").and_then(|v| v.as_number()).map(|v| (v as u8).min(2));
    gs.ml = dict.get("ML").and_then(|v| v.as_number()).map(|v| v as f32);
    if let Some(items) = dict.get("D").and_then(|v| v.as_array()) {
      if let Some(PdfValue::Array(dash)) = items.first() {
        let array: Vec<f32> = dash.iter().filter_map(|v| v.as_number().map(|n| (n as f32).max(0.0))).collect();
        let phase = items.get(1).and_then(|v| v.as_number()).unwrap_or(0.0) as f32;
        gs.dash = Some((array, phase));
      }
    }
    gs.ca = dict.get("ca").and_then(|v| v.as_number()).map(|v| v as f32);
    gs.ca_stroke = dict.get("CA").and_then(|v| v.as_number()).map(|v| v as f32);
    gs.blend = dict.get("BM").and_then(|v| v.as_name()).map(str::to_owned);
    Some(gs)
  }

  fn special_color(&self, space: &str, comps: &[f32]) -> Option<Rgb> {
    let value = self.parser.resource_entry(&self.resources, "ColorSpace", space)?;
    self.eval_space(&value, comps)
  }

  fn shading(&self, name: &str) -> Option<ResolvedShading> {
    let dict = self.parser.resource_entry(&self.resources, "Shading", name)?;
    self.resolve_shading_dict(&dict)
  }

  fn pattern(&self, name: &str) -> Option<ResolvedPattern> {
    let dict = self.parser.resource_entry(&self.resources, "Pattern", name)?;
    let kind = dict.get("PatternType").and_then(|v| v.as_number()).unwrap_or(1.0) as i64;
    match kind {
      1 => Some(ResolvedPattern::Tiling),
      2 => {
        let shading = dict.get("Shading")?;
        let resolved = self.parser.resolve_value(shading).ok()?;
        Some(ResolvedPattern::Shading(self.resolve_shading_dict(&resolved).unwrap_or(ResolvedShading::Unsupported)))
      }
      _ => None,
    }
  }
}

impl<'a> DocProvider<'a> {
  fn num(dict: &PdfValue, key: &str, fallback: f32) -> f32 {
    dict.get(key).and_then(|v| v.as_number()).unwrap_or(fallback as f64) as f32
  }

  fn num_vec(dict: &PdfValue, key: &str, fallback: &[f32]) -> Vec<f32> {
    match dict.get(key).and_then(|v| v.as_array()) {
      Some(items) => items.iter().filter_map(|v| v.as_number().map(|n| n as f32)).collect(),
      None => fallback.to_vec(),
    }
  }

  /// Evaluate a resolved color space value with components to sRGB.
  fn eval_space(&self, value: &PdfValue, comps: &[f32]) -> Option<Rgb> {
    match value {
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
        "Pattern" => None,
        _ => None,
      },
      PdfValue::Array(items) => {
        let head = items.first()?.as_name()?.to_owned();
        match head.as_str() {
          "CalGray" => {
            let dict = self.parser.resolve_value(&items[1]).ok()?;
            let gamma = Self::num(&dict, "Gamma", 1.0);
            Some(calgray_to_rgb(comps.first().copied().unwrap_or(0.0), gamma))
          }
          "CalRGB" => {
            let dict = self.parser.resolve_value(&items[1]).ok()?;
            let gamma = Self::num_vec(&dict, "Gamma", &[1.0, 1.0, 1.0]);
            let matrix = Self::num_vec(&dict, "Matrix", &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
            Some(calrgb_to_rgb(
              [
                comps.first().copied().unwrap_or(0.0),
                comps.get(1).copied().unwrap_or(0.0),
                comps.get(2).copied().unwrap_or(0.0),
              ],
              [gamma.first().copied().unwrap_or(1.0), gamma.get(1).copied().unwrap_or(1.0), gamma.get(2).copied().unwrap_or(1.0)],
              [
                matrix.first().copied().unwrap_or(1.0),
                matrix.get(1).copied().unwrap_or(0.0),
                matrix.get(2).copied().unwrap_or(0.0),
                matrix.get(3).copied().unwrap_or(0.0),
                matrix.get(4).copied().unwrap_or(1.0),
                matrix.get(5).copied().unwrap_or(0.0),
                matrix.get(6).copied().unwrap_or(0.0),
                matrix.get(7).copied().unwrap_or(0.0),
                matrix.get(8).copied().unwrap_or(1.0),
              ],
            ))
          }
          "Lab" => {
            let dict = self.parser.resolve_value(&items[1]).ok()?;
            let range = Self::num_vec(&dict, "Range", &[-100.0, 100.0, -100.0, 100.0]);
            let l = comps.first().copied().unwrap_or(0.0).clamp(0.0, 100.0);
            let a = comps.get(1).copied().unwrap_or(0.0).clamp(range.first().copied().unwrap_or(-100.0), range.get(1).copied().unwrap_or(100.0));
            let b = comps.get(2).copied().unwrap_or(0.0).clamp(range.get(2).copied().unwrap_or(-100.0), range.get(3).copied().unwrap_or(100.0));
            Some(lab_to_rgb([l, a, b]))
          }
          "ICCBased" => {
            let target = items.get(1)?;
            let (num, dict) = match target {
              PdfValue::Ref(n, _) => (*n, self.parser.object(*n).ok()?.value),
              other => (0, self.parser.resolve_value(other).ok()?),
            };
            let n = dict.get("N").and_then(|v| v.as_number()).unwrap_or(3.0) as usize;
            if let Some(alt) = dict.get("Alternate") {
              let resolved = self.parser.resolve_value(alt).ok()?;
              if self.eval_space(&resolved, comps).is_some() {
                return self.eval_space(&resolved, comps);
              }
            }
            let _ = num;
            match n {
              1 => {
                let g = comps.first().copied().unwrap_or(0.0);
                Some(Rgb { r: g, g, b: g })
              }
              4 => Some(crate::graphics::cmyk_to_rgb(
                comps.first().copied().unwrap_or(0.0),
                comps.get(1).copied().unwrap_or(0.0),
                comps.get(2).copied().unwrap_or(0.0),
                comps.get(3).copied().unwrap_or(0.0),
              )),
              _ => Some(Rgb {
                r: comps.first().copied().unwrap_or(0.0),
                g: comps.get(1).copied().unwrap_or(0.0),
                b: comps.get(2).copied().unwrap_or(0.0),
              }),
            }
          }
          "Separation" => {
            let alt = self.parser.resolve_value(items.get(2)?).ok()?;
            let tint_fn = self.parser.resolve_value(items.get(3)?).ok()?;
            let func = parse_function(&tint_fn).ok()?;
            let out = func.eval(&[comps.first().copied().unwrap_or(0.0)]);
            if out.is_empty() {
              return None;
            }
            self.eval_space(&alt, &out)
          }
          "DeviceN" => {
            let alt = self.parser.resolve_value(items.get(2)?).ok()?;
            let tint_fn = self.parser.resolve_value(items.get(3)?).ok()?;
            let func = parse_function(&tint_fn).ok()?;
            let count = items.get(1).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(comps.len());
            let mut input: Vec<f32> = comps.to_vec();
            input.resize(count.max(1), 0.0);
            let out = func.eval(&input);
            if out.is_empty() {
              return None;
            }
            self.eval_space(&alt, &out)
          }
          "Indexed" => {
            let base = self.parser.resolve_value(items.get(1)?).ok()?;
            let hival = items.get(2).and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
            let lookup = items.get(3)?;
            let table: Vec<u8> = match lookup {
              PdfValue::Str(bytes) | PdfValue::Hex(bytes) => bytes.clone(),
              PdfValue::Ref(n, _) => {
                let (dict, bytes) = self.parser.decoded_stream(*n).ok()?;
                let _ = dict;
                bytes
              }
              _ => return None,
            };
            let per = match &base {
              PdfValue::Name(n) if n == "DeviceGray" || n == "G" => 1,
              PdfValue::Name(n) if n == "DeviceRGB" || n == "RGB" => 3,
              PdfValue::Name(n) if n == "DeviceCMYK" || n == "CMYK" => 4,
              PdfValue::Array(_) => {
                // Indexed over an array space: evaluate one entry.
                let entry = indexed_lookup(&table, hival, comps.first().copied().unwrap_or(0.0) as usize, 1);
                return self.eval_space(&base, &entry);
              }
              _ => return None,
            };
            let entry = indexed_lookup(&table, hival, comps.first().copied().unwrap_or(0.0) as usize, per);
            self.eval_space(&base, &entry)
          }
          "Pattern" => None,
          _ => None,
        }
      }
      _ => None,
    }
    .map(|rgb| rgb.clamp())
  }

  fn resolve_shading_dict(&self, dict: &PdfValue) -> Option<ResolvedShading> {
    let kind = dict.get("ShadingType").and_then(|v| v.as_number()).unwrap_or(0.0) as i64;
    let coords: Vec<f32> = dict
      .get("Coords")
      .and_then(|v| v.as_array())
      .map(|a| a.iter().filter_map(|v| v.as_number().map(|n| n as f32)).collect())
      .unwrap_or_default();
    let extend: [bool; 2] = match dict.get("Extend").and_then(|v| v.as_array()) {
      Some(a) => [
        a.first().and_then(|v| match v {
          PdfValue::Bool(b) => Some(*b),
          _ => None,
        }).unwrap_or(true),
        a.get(1).and_then(|v| match v {
          PdfValue::Bool(b) => Some(*b),
          _ => None,
        }).unwrap_or(true),
      ],
      None => [true, true],
    };
    let cs = dict.get("ColorSpace").and_then(|v| self.parser.resolve_value(v).ok()).unwrap_or(PdfValue::Name("DeviceRGB".into()));
    let func_value = dict.get("Function").and_then(|v| self.parser.resolve_value(v).ok())?;
    let mut func = parse_function(&func_value).ok()?;
    // Sampled functions carry their table in the stream.
    if let PdfValue::Dict(_) = func_value {
      let is_sampled = func_value.get("FunctionType").and_then(|v| v.as_number()).unwrap_or(-1.0) == 0.0;
      if is_sampled {
        func = Function::Unsupported;
      }
    }
    if matches!(func, Function::Unsupported) {
      // Try stream-backed sampled functions via shading dict refs.
      if let Some(PdfValue::Ref(n, _)) = dict.get("Function") {
        if let Ok((sdict, bytes)) = self.parser.decoded_stream(*n) {
          if let Ok(sampled) = parse_sampled(&sdict, &bytes) {
            func = sampled;
          }
        }
      }
    }
    if matches!(func, Function::Unsupported) {
      return Some(ResolvedShading::Unsupported);
    }
    let stops = shading_stops(&func, &|out| self.eval_space(&cs, out).unwrap_or(Rgb::black()), 33);
    match kind {
      2 if coords.len() >= 4 => Some(ResolvedShading::Axial {
        coords: [coords[0], coords[1], coords[2], coords[3]],
        stops,
        extend,
      }),
      3 if coords.len() >= 6 => Some(ResolvedShading::Radial {
        coords: [coords[0], coords[1], coords[2], coords[3], coords[4], coords[5]],
        stops,
        extend,
      }),
      _ => Some(ResolvedShading::Unsupported),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parser::tests::minimal_pdf;

  #[test]
  fn loads_single_page_document() {
    let pdf = minimal_pdf(b"BT /F1 12 Tf 72 720 Td (Hello PDF) Tj ET");
    let doc = PdfDocument::load_bytes(pdf).unwrap();
    assert_eq!(doc.page_count(), 1);
    assert!(doc.page(0).unwrap().text().contains("Hello PDF"));
  }

  #[test]
  fn rejects_bad_page_index() {
    let pdf = minimal_pdf(b"BT /F1 12 Tf (x) Tj ET");
    let doc = PdfDocument::load_bytes(pdf).unwrap();
    assert_eq!(doc.page(7).unwrap_err(), PdfError::PageOutOfRange(7));
  }

  #[test]
  fn rejects_non_pdf() {
    assert_eq!(PdfDocument::load_bytes(b"hello".to_vec()).unwrap_err(), PdfError::InvalidHeader);
  }
}
