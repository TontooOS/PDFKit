use std::collections::HashMap;

use crate::annot::{DocInfo, LinkTarget, Outline, parse_annotation, parse_info, parse_outlines, pdfdoc_to_string};
use crate::color::{Function, ResolvedPattern, ResolvedShading, calgray_to_rgb, calrgb_to_rgb, indexed_lookup, lab_to_rgb, parse_function, parse_sampled, shading_stops};
use crate::error::{PdfError, Result};
use crate::font::{DecoderKind, FontDecoder, apply_differences, parse_cmap};
use crate::graphics::{
  ExtGState, FillRule, FontInfo, InlineVal, Matrix, MAX_FORM_DEPTH, PathItem, PathSeg, PlacedImage, ResourceProvider,
  Rgb, XObjectResult, interpret_spanned, text_runs,
};
use crate::image::{apply_alpha, apply_constant_alpha, decode_jpeg, decode_mask_alpha, decode_samples, decode_smask_alpha};
use crate::objects::PdfValue;
use crate::page::PdfPage;
use crate::parser::{ContentSegment, FileParser};

/// A loaded PDF document: parsed structure plus interpreted pages.
///
/// The document owns its bytes so pages can be re-interpreted on
/// demand (needed later for the editor without re-reading the file).
/// Pages parse eagerly at load; large documents stream later.
#[derive(Debug)]
pub struct PdfDocument {
  pages: Vec<PdfPage>,
  outlines: Vec<Outline>,
  info: DocInfo,
}

impl PdfDocument {
  /// Parse a document from memory.
  pub fn load_bytes(data: Vec<u8>) -> Result<Self> {
    Self::load_bytes_with_password(data, "")
  }

  /// Parse a document from memory with a password (may be empty).
  /// Encrypted files without a usable password yield `NeedsPassword`
  /// (empty password) or `WrongPassword`.
  pub fn load_bytes_with_password(data: Vec<u8>, password: &str) -> Result<Self> {
    let parser = FileParser::new_with_password(data, password.as_bytes())?;
    Self::from_parser(&parser)
  }

  /// Parse a document from a file path.
  pub fn load_file(path: &str) -> Result<Self> {
    Self::load_file_with_password(path, "")
  }

  /// Parse a document from a file path with a password.
  pub fn load_file_with_password(path: &str, password: &str) -> Result<Self> {
    let data = std::fs::read(path).map_err(|e| PdfError::InvalidObject(e.to_string()))?;
    Self::load_bytes_with_password(data, password)
  }

  fn from_parser(parser: &FileParser) -> Result<Self> {
    let parsed = parser.pages()?;
    let page_of: HashMap<u32, usize> = parsed.iter().map(|p| (p.objnum, p.index)).collect();
    let names = collect_names(parser);
    let resolve_page = |v: &PdfValue| match v {
      PdfValue::Ref(n, _) => page_of.get(n).copied(),
      _ => None,
    };
    let mut pages = Vec::with_capacity(parsed.len());
    for item in &parsed {
      let builder = FontBuilder { parser };
      let mut fonts = HashMap::new();
      for font in &item.fonts {
        let info = builder.info(&item.resources, &font.resource);
        fonts.insert(font.resource.clone(), info);
      }
      let provider = DocProvider { parser, resources: item.resources.clone(), fonts };
      let items = interpret_spanned(&item.content, provider, Matrix::ident(), 0, &item.content_segments)?;
      let runs = text_runs(&items);
      let width = (item.media_box[2] - item.media_box[0]).max(1.0);
      let height = (item.media_box[3] - item.media_box[1]).max(1.0);
      let annotations = item
        .annots
        .iter()
        .filter_map(|a| parser.resolve_value(a).ok())
        .filter_map(|d| parse_annotation(&d, &resolve_page))
        .collect();
      pages.push(PdfPage {
        number: item.index,
        width,
        height,
        origin_x: item.media_box[0],
        origin_y: item.media_box[1],
        runs,
        items,
        annotations,
        rotate: item.rotate,
        content: item.content.clone(),
        content_segments: item.content_segments.clone(),
      });
    }
    if pages.is_empty() {
      return Err(PdfError::NoPages);
    }
    let outlines = parser
      .catalog()
      .ok()
      .and_then(|catalog| catalog.get("Outlines").cloned())
      .and_then(|o| parser.resolve_value(&o).ok())
      .map(|outlines| {
        parse_outlines(
          &outlines,
          &|v| parser.resolve_value(v).ok(),
          &|v| resolve_dest(parser, &names, &page_of, v).and_then(|t| match t {
            LinkTarget::Page(i) => Some(i),
            _ => None,
          }),
        )
      })
      .unwrap_or_default();
    let outlines = outlines
      .into_iter()
      .map(|o| resolve_outline(parser, &names, &page_of, o))
      .collect();
    let info = parser.info_dict().and_then(|d| parser.resolve_value(&d).ok()).map(|d| parse_info(&d)).unwrap_or_default();
    Ok(Self { pages, outlines, info })
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

  /// Bookmark outlines with resolved page targets.
  pub fn outlines(&self) -> &[Outline] {
    &self.outlines
  }

  /// Document metadata from the trailer `/Info` dict.
  pub fn info(&self) -> &DocInfo {
    &self.info
  }
}

/// Collect named destinations (`/Names /Dests` tree plus old-style
/// catalog `/Dests`) into a flat map.
fn collect_names(parser: &FileParser) -> HashMap<String, PdfValue> {
  let mut out = HashMap::new();
  let catalog = match parser.catalog() {
    Ok(catalog) => catalog,
    Err(_) => return out,
  };
  if let Some(dests) = catalog.get("Dests").and_then(|v| parser.resolve_value(v).ok()) {
    if let PdfValue::Dict(entries) = &dests {
      for (name, value) in entries {
        out.insert(name.clone(), value.clone());
      }
    }
  }
  if let Some(names) = catalog.get("Names").and_then(|v| parser.resolve_value(v).ok()) {
    if let Some(tree) = names.get("Dests").and_then(|v| parser.resolve_value(v).ok()) {
      collect_tree(parser, &tree, &mut out);
    }
  }
  out
}

fn collect_tree(parser: &FileParser, node: &PdfValue, out: &mut HashMap<String, PdfValue>) {
  if let Some(items) = node.get("Names").and_then(|v| v.as_array()) {
    for pair in items.chunks(2) {
      if pair.len() == 2 {
        let key = match &pair[0] {
          PdfValue::Str(bytes) | PdfValue::Hex(bytes) => pdfdoc_to_string(bytes),
          PdfValue::Name(n) => n.clone(),
          _ => continue,
        };
        out.insert(key, pair[1].clone());
      }
    }
  }
  if let Some(kids) = node.get("Kids").and_then(|v| v.as_array()) {
    for kid in kids {
      if let Ok(resolved) = parser.resolve_value(kid) {
        collect_tree(parser, &resolved, out);
      }
    }
  }
}

/// Resolve a destination value to a link target.
fn resolve_dest(parser: &FileParser, names: &HashMap<String, PdfValue>, page_of: &HashMap<u32, usize>, dest: &PdfValue) -> Option<LinkTarget> {
  match dest {
    PdfValue::Ref(n, _) => match page_of.get(n) {
      Some(index) => Some(LinkTarget::Page(*index)),
      None => {
        let resolved = parser.resolve_value(dest).ok()?;
        resolve_dest(parser, names, page_of, &resolved)
      }
    },
    PdfValue::Name(n) => match names.get(n) {
      Some(mapped) => resolve_dest(parser, names, page_of, mapped),
      None => {
        if let Ok(page) = n.parse::<usize>() {
          Some(LinkTarget::Page(page))
        } else {
          Some(LinkTarget::Named(n.clone()))
        }
      }
    },
    PdfValue::Str(bytes) | PdfValue::Hex(bytes) => {
      let name = pdfdoc_to_string(bytes);
      match names.get(&name) {
        Some(mapped) => resolve_dest(parser, names, page_of, mapped),
        None => {
          if let Ok(page) = name.parse::<usize>() {
            Some(LinkTarget::Page(page))
          } else {
            Some(LinkTarget::Named(name))
          }
        }
      }
    }
    PdfValue::Array(items) => match items.first() {
      Some(PdfValue::Ref(n, _)) => page_of.get(n).copied().map(LinkTarget::Page),
      Some(first) if first.as_number().is_some() => Some(LinkTarget::Page(first.as_number().unwrap_or(0.0) as usize)),
      _ => None,
    },
    _ => None,
  }
}

fn resolve_outline(parser: &FileParser, names: &HashMap<String, PdfValue>, page_of: &HashMap<u32, usize>, item: Outline) -> Outline {
  let target = item.target.and_then(|t| match t {
    LinkTarget::Named(n) => {
      let key = PdfValue::Name(n);
      resolve_dest(parser, names, page_of, &key).or(Some(LinkTarget::Named(match &key {
        PdfValue::Name(s) => s.clone(),
        _ => String::new(),
      })))
    }
    other => Some(other),
  });
  Outline {
    target,
    children: item.children.into_iter().map(|c| resolve_outline(parser, names, page_of, c)).collect(),
    ..item
  }
}

/// Builds full `FontInfo` values (encoding, ToUnicode, widths)
///
/// from page font resources.
struct FontBuilder<'a> {
  parser: &'a FileParser,
}

impl<'a> FontBuilder<'a> {
  fn info(&self, resources: &PdfValue, resource: &str) -> FontInfo {
    let fallback = FontInfo::simple(resource, "Unknown");
    let font_ref = resources
      .get("Font")
      .and_then(|fonts| self.parser.resolve_value(fonts).ok())
      .and_then(|fonts| fonts.get(resource).cloned())
      .and_then(|entry| self.parser.resolve_value(&entry).ok());
    let dict = match font_ref {
      Some(PdfValue::Dict(_)) => font_ref.unwrap(),
      _ => return fallback,
    };
    let subtype = dict.get("Subtype").and_then(|v| v.as_name()).unwrap_or("").to_owned();
    let base_font = dict.get("BaseFont").and_then(|v| v.as_name()).unwrap_or("Unknown").to_owned();
    let mut bold = base_font.to_lowercase().contains("bold");
    let mut italic =
      base_font.to_lowercase().contains("italic") || base_font.to_lowercase().contains("oblique");
    let mut missing_width = 500.0;
    if let Some(desc) = dict.get("FontDescriptor").and_then(|v| self.parser.resolve_value(v).ok()) {
      let flags = desc.get("Flags").and_then(|v| v.as_number()).unwrap_or(0.0) as u32;
      if flags & (1 << 18) != 0 {
        bold = true;
      }
      if flags & (1 << 6) != 0 {
        italic = true;
      }
      missing_width = desc.get("MissingWidth").and_then(|v| v.as_number()).unwrap_or(500.0) as f32;
    }
    // ToUnicode CMap (top level, also used by Type0).
    let cmap = dict
      .get("ToUnicode")
      .and_then(|v| match v {
        PdfValue::Ref(n, _) => Some(*n),
        _ => None,
      })
      .and_then(|n| self.parser.decoded_stream(n).ok())
      .and_then(|(_, bytes)| parse_cmap(&bytes).ok());
    if subtype == "Type0" {
      return self.cid_font(resource, &base_font, bold, italic, &dict, cmap, missing_width);
    }
    self.simple_font(resource, &base_font, bold, italic, &dict, cmap, missing_width)
  }

  fn simple_font(
    &self,
    resource: &str,
    base_font: &str,
    bold: bool,
    italic: bool,
    dict: &PdfValue,
    cmap: Option<crate::font::CMap>,
    missing_width: f32,
  ) -> FontInfo {
    let _ = bold;
    let (base_name, diffs) = match dict.get("Encoding").and_then(|v| self.parser.resolve_value(v).ok()) {
      Some(PdfValue::Name(name)) => (name, vec![]),
      Some(PdfValue::Dict(_)) => {
        let enc = dict.get("Encoding").and_then(|v| self.parser.resolve_value(v).ok()).unwrap_or(PdfValue::Null);
        let base = enc.get("BaseEncoding").and_then(|v| v.as_name()).unwrap_or("WinAnsiEncoding").to_owned();
        (base, Self::differences(&enc))
      }
      _ => ("WinAnsiEncoding".into(), vec![]),
    };
    let mut decoder = FontDecoder::simple_named(&base_name);
    if !diffs.is_empty() {
      if let DecoderKind::Simple(ref mut table) = decoder.kind {
        apply_differences(table, &diffs);
      }
    }
    // Widths for FirstChar..LastChar.
    let first = dict.get("FirstChar").and_then(|v| v.as_number()).unwrap_or(0.0) as u32;
    if let Some(items) = dict.get("Widths").and_then(|v| v.as_array()) {
      for (i, item) in items.iter().enumerate() {
        if let Some(w) = item.as_number() {
          decoder.widths.insert(first + i as u32, w as f32);
        }
      }
    }
    decoder.default_width = missing_width;
    if let Some(cmap) = cmap {
      decoder.kind = DecoderKind::CMap(cmap);
    }
    FontInfo { resource: resource.into(), base_font: base_font.into(), italic, decoder }
  }

  fn cid_font(
    &self,
    resource: &str,
    base_font: &str,
    bold: bool,
    italic: bool,
    dict: &PdfValue,
    cmap: Option<crate::font::CMap>,
    missing_width: f32,
  ) -> FontInfo {
    let _ = bold;
    let descendant = dict
      .get("DescendantFonts")
      .and_then(|v| v.as_array())
      .and_then(|a| a.first().cloned())
      .and_then(|v| self.parser.resolve_value(&v).ok());
    let mut widths = HashMap::new();
    let mut default_width = 1000.0;
    if let Some(cid) = descendant {
      default_width = cid.get("DW").and_then(|v| v.as_number()).unwrap_or(1000.0) as f32;
      if let Some(items) = cid.get("W").and_then(|v| v.as_array()) {
        let mut i = 0;
        while i < items.len() {
          let lo = items.get(i).and_then(|v| v.as_number()).unwrap_or(0.0) as u32;
          match items.get(i + 1) {
            Some(PdfValue::Array(list)) => {
              for (k, w) in list.iter().enumerate() {
                if let Some(width) = w.as_number() {
                  widths.insert(lo + k as u32, width as f32);
                }
              }
              i += 2;
            }
            Some(end) => {
              let hi = end.as_number().unwrap_or(lo as f64) as u32;
              let w = items.get(i + 2).and_then(|v| v.as_number()).unwrap_or(1000.0) as f32;
              for code in lo..=hi.min(lo + 10000) {
                widths.insert(code, w);
              }
              i += 3;
            }
            None => break,
          }
        }
      }
      if let Some(desc) = cid.get("FontDescriptor").and_then(|v| self.parser.resolve_value(v).ok()) {
        default_width = desc.get("MissingWidth").and_then(|v| v.as_number()).unwrap_or(default_width as f64) as f32;
      }
    }
    let _ = missing_width;
    let kind = match cmap {
      Some(cmap) => DecoderKind::CMap(cmap),
      None => DecoderKind::Identity,
    };
    FontInfo { resource: resource.into(), base_font: base_font.into(), italic, decoder: FontDecoder { kind, widths, default_width } }
  }

  fn differences(enc: &PdfValue) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    if let Some(items) = enc.get("Differences").and_then(|v| v.as_array()) {
      let mut code = 0u32;
      for item in items {
        match item {
          PdfValue::Number(n) => code = *n as u32,
          PdfValue::Name(name) => {
            out.push((code, name.clone()));
            code += 1;
          }
          _ => {}
        }
      }
    }
    out
  }
}
/// Document-backed resources for one page: fonts plus reference
/// resolution for ExtGState, color spaces, shadings and patterns.
struct DocProvider<'a> {
  parser: &'a FileParser,
  resources: PdfValue,
  fonts: HashMap<String, FontInfo>,
}
impl<'a> ResourceProvider for DocProvider<'a> {
  fn font(&self, name: &str) -> Option<FontInfo> {
    self.fonts.get(name).cloned()
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

  fn xobject(&self, name: &str, ctm: Matrix, fill: Rgb, alpha: f32, depth: u32) -> XObjectResult {
    if depth >= MAX_FORM_DEPTH {
      return XObjectResult::Skipped(String::from("form depth limit"));
    }
    // XObjects must be indirect (streams); resolve the dict chain
    // but keep the final reference to learn the object number.
    let entry = self
      .parser
      .resolve_value(&self.resources)
      .ok()
      .and_then(|r| r.get("XObject").cloned())
      .and_then(|x| self.parser.resolve_value(&x).ok())
      .and_then(|d| d.get(name).cloned());
    let num = match entry {
      Some(PdfValue::Ref(n, _)) => n,
      _ => return XObjectResult::Skipped(String::from("missing XObject")),
    };
    let obj = match self.parser.object(num) {
      Ok(obj) => obj,
      Err(_) => return XObjectResult::Skipped(String::from("missing XObject")),
    };
    let raw = match obj.stream {
      Some(raw) => raw,
      None => return XObjectResult::Skipped(String::from("XObject without stream")),
    };
    let subtype = obj.value.get("Subtype").and_then(|v| v.as_name()).unwrap_or("").to_owned();
    match subtype.as_str() {
      "Form" => self.place_form(&obj.value, &raw, ctm, depth, num),
      "Image" => match self.decode_image(&obj.value, &raw, fill, alpha) {
        Some(image) => XObjectResult::Image(PlacedImage { image, ctm, src: None }),
        None => XObjectResult::Skipped(String::from("unsupported image")),
      },
      _ => XObjectResult::Skipped(String::from("unknown XObject subtype")),
    }
  }

  fn inline_image(
    &self,
    dict: &[(String, InlineVal)],
    data: &[u8],
    ctm: Matrix,
    fill: Rgb,
    alpha: f32,
  ) -> XObjectResult {
    let value = inline_dict(dict);
    let filters = filter_names(&value);
    if filters.iter().any(|f| f == "JPXDecode" || f == "CCITTFaxDecode" || f == "CCF" || f == "JBIG2Decode") {
      return XObjectResult::Skipped(String::from("unsupported inline filter"));
    }
    if filters.iter().any(|f| f == "DCTDecode" || f == "DCT") {
      return match decode_jpeg(data) {
        Some(mut image) => {
          apply_constant_alpha(&mut image.rgba, alpha);
          XObjectResult::Image(PlacedImage { image, ctm, src: None })
        }
        None => XObjectResult::Skipped(String::from("bad JPEG data")),
      };
    }
    let samples = match crate::filter::decode(&value, data) {
      Ok(samples) => samples,
      Err(_) => return XObjectResult::Skipped(String::from("inline filter failed")),
    };
    match self.decode_image(&value, &samples, fill, alpha) {
      Some(image) => XObjectResult::Image(PlacedImage { image, ctm, src: None }),
      None => XObjectResult::Skipped(String::from("unsupported inline image")),
    }
  }
}

impl<'a> DocProvider<'a> {
  fn place_form(&self, dict: &PdfValue, raw: &[u8], ctm: Matrix, depth: u32, objnum: u32) -> XObjectResult {
    let content = match crate::filter::decode(dict, raw) {
      Ok(content) => content,
      Err(_) => return XObjectResult::Skipped(String::from("form filter failed")),
    };
    let matrix = dict
      .get("Matrix")
      .and_then(|v| v.as_array())
      .map(|a| {
        let n: Vec<f32> = a.iter().filter_map(|v| v.as_number().map(|n| n as f32)).collect();
        Matrix {
          a: n.first().copied().unwrap_or(1.0),
          b: n.get(1).copied().unwrap_or(0.0),
          c: n.get(2).copied().unwrap_or(0.0),
          d: n.get(3).copied().unwrap_or(1.0),
          e: n.get(4).copied().unwrap_or(0.0),
          f: n.get(5).copied().unwrap_or(0.0),
        }
      })
      .unwrap_or_else(Matrix::ident);
    let base = matrix.concat(ctm);
    let resources = dict
      .get("Resources")
      .and_then(|v| self.parser.resolve_value(v).ok())
      .unwrap_or_else(|| self.resources.clone());
    let sub = DocProvider { parser: self.parser, resources, fonts: self.fonts.clone() };
    // The form body is its own content stream: tag its spans with the
    // form object number so an editor can address its bytes.
    let segments = [ContentSegment { obj: objnum, start: 0, end: content.len() }];
    let mut items = match interpret_spanned(&content, sub, base, depth + 1, &segments) {
      Ok(items) => items,
      Err(_) => return XObjectResult::Skipped(String::from("form content failed")),
    };
    // Clip to the form BBox inside a Save/Restore frame.
    if let Some(bbox) = dict.get("BBox").and_then(|v| v.as_array()) {
      let n: Vec<f32> = bbox.iter().filter_map(|v| v.as_number().map(|n| n as f32)).collect();
      if n.len() >= 4 {
        let (x0, y0, x1, y1) = (n[0], n[1], n[2], n[3]);
        let mut framed = vec![
          crate::graphics::PageItem::Save,
          crate::graphics::PageItem::Path(PathItem {
            subpaths: vec![vec![
              PathSeg::Move(x0, y0),
              PathSeg::Line(x1, y0),
              PathSeg::Line(x1, y1),
              PathSeg::Line(x0, y1),
              PathSeg::Close,
            ]],
            ctm: base,
            fill: None,
            fill_alpha: 1.0,
            stroke: None,
            stroke_alpha: 1.0,
            clip: Some(FillRule::NonZero),
            src: None,
          }),
        ];
        framed.append(&mut items);
        framed.push(crate::graphics::PageItem::Restore);
        items = framed;
      }
    }
    XObjectResult::Items(items)
  }

  fn decode_image(&self, dict: &PdfValue, raw: &[u8], fill: Rgb, alpha: f32) -> Option<crate::image::DecodedImage> {
    // Stencil masks paint with the current fill color. `/ImageMask`
    // is a boolean in real files (numbers only in sloppy writers).
    let masked = match dict.get("ImageMask") {
      Some(PdfValue::Bool(b)) => *b,
      Some(v) => v.as_number().is_some_and(|n| n != 0.0),
      None => false,
    };
    if masked {
      let samples = crate::filter::decode(dict, raw).ok()?;
      let plane = decode_mask_alpha(dict, &samples)?;
      let width = dict.get("Width").and_then(|v| v.as_number()).unwrap_or(0.0) as u32;
      let height = dict.get("Height").and_then(|v| v.as_number()).unwrap_or(0.0) as u32;
      let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
      for a in &plane {
        rgba.push((fill.r.clamp(0.0, 1.0) * 255.0) as u8);
        rgba.push((fill.g.clamp(0.0, 1.0) * 255.0) as u8);
        rgba.push((fill.b.clamp(0.0, 1.0) * 255.0) as u8);
        rgba.push(*a);
      }
      let mut image = crate::image::DecodedImage { width, height, rgba, interpolate: false };
      apply_constant_alpha(&mut image.rgba, alpha);
      return Some(image);
    }
    let filters = stream_filters(dict);
    if filters.iter().any(|f| f == "JPXDecode" || f == "CCITTFaxDecode" || f == "CCF" || f == "JBIG2Decode") {
      return None;
    }
    if filters.iter().any(|f| f == "DCTDecode" || f == "DCT") {
      let mut image = decode_jpeg(raw)?;
      let (w, h) = (image.width, image.height);
      let want_w = dict.get("Width").and_then(|v| v.as_number()).unwrap_or(w as f64) as u32;
      let want_h = dict.get("Height").and_then(|v| v.as_number()).unwrap_or(h as f64) as u32;
      if want_w != 0 && want_h != 0 && (want_w != w || want_h != h) {
        return None;
      }
      if let Some(smask) = self.smask_alpha(dict) {
        if smask.len() == (w * h) as usize {
          apply_alpha(&mut image.rgba, &smask);
        }
      }
      apply_constant_alpha(&mut image.rgba, alpha);
      return Some(image);
    }
    let samples = crate::filter::decode(dict, raw).ok()?;
    let map_space = dict.get("ColorSpace").and_then(|v| self.parser.resolve_value(v).ok());
    let mut image = decode_samples(dict, &samples, &|comps| {
      map_space.as_ref().and_then(|space| self.eval_space(space, comps))
    })?;
    if let Some(smask) = self.smask_alpha(dict) {
      if smask.len() == (image.width * image.height) as usize {
        apply_alpha(&mut image.rgba, &smask);
      }
    }
    apply_constant_alpha(&mut image.rgba, alpha);
    Some(image)
  }

  fn smask_alpha(&self, dict: &PdfValue) -> Option<Vec<u8>> {
    let target = dict.get("SMask")?;
    let num = target.as_ref().map(|(n, _)| n)?;
    // decoded_stream already returns decoded bytes; running the
    // filters again would corrupt the mask (coverage E079 then
    // renders as if unmasked).
    let (sdict, samples) = self.parser.decoded_stream(num).ok()?;
    let map_space = sdict.get("ColorSpace").and_then(|v| self.parser.resolve_value(v).ok());
    decode_smask_alpha(&sdict, &samples, &|comps| {
      map_space.as_ref().and_then(|space| self.eval_space(space, comps))
    })
  }
}

/// Filter names of a stream dict (direct only; resolved by callers).
fn stream_filters(dict: &PdfValue) -> Vec<String> {
  match dict.get("Filter") {
    None | Some(PdfValue::Null) => vec![],
    Some(PdfValue::Name(name)) => vec![filter_full_name(name)],
    Some(PdfValue::Array(items)) => {
      items.iter().filter_map(|v| v.as_name().map(filter_full_name)).collect()
    }
    _ => vec![],
  }
}

fn filter_full_name(name: &str) -> String {
  match name {
    "AHx" => "ASCIIHexDecode",
    "A85" => "ASCII85Decode",
    "LZW" => "LZWDecode",
    "Fl" => "FlateDecode",
    "RL" => "RunLengthDecode",
    "CCF" => "CCITTFaxDecode",
    "DCT" => "DCTDecode",
    other => other,
  }
  .into()
}

fn filter_names(value: &PdfValue) -> Vec<String> {
  stream_filters(value)
}

/// Build a stream-style dict value from neutral inline-image entries.
fn inline_dict(dict: &[(String, InlineVal)]) -> PdfValue {
  let mut entries = Vec::new();
  for (key, value) in dict {
    let full = match key.as_str() {
      "BPC" => "BitsPerComponent",
      "CS" => "ColorSpace",
      "D" => "Decode",
      "DP" => "DecodeParms",
      "F" => "Filter",
      "H" => "Height",
      "W" => "Width",
      "I" => "Interpolate",
      "IM" => "ImageMask",
      other => other,
    };
    entries.push((full.into(), inline_value(value)));
  }
  PdfValue::Dict(entries)
}

fn inline_value(value: &InlineVal) -> PdfValue {
  match value {
    InlineVal::Name(n) => match n.as_str() {
      "G" => PdfValue::Name("DeviceGray".into()),
      "RGB" => PdfValue::Name("DeviceRGB".into()),
      "CMYK" => PdfValue::Name("DeviceCMYK".into()),
      "I" => PdfValue::Name("Indexed".into()),
      "true" => PdfValue::Bool(true),
      "false" => PdfValue::Bool(false),
      _ => PdfValue::Name(inline_filter_value(n)),
    },
    InlineVal::Num(n) => PdfValue::Number(*n),
    InlineVal::Array(items) => PdfValue::Array(items.iter().map(inline_value).collect()),
    InlineVal::Str(bytes) => PdfValue::Str(bytes.clone()),
  }
}

fn inline_filter_value(name: &str) -> String {
  filter_full_name(name)
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

  /// PDF with a `/Contents` array of two streams, so page content is
  /// the concatenation of two separately addressable objects.
  fn two_stream_pdf(first: &[u8], second: &[u8]) -> Vec<u8> {
    let stream = |body: &[u8]| {
      [b"<< /Length ".to_vec(), body.len().to_string().into_bytes(), b" >>\nstream\n".to_vec(), body.to_vec(), b"\nendstream".to_vec()].concat()
    };
    let objects: Vec<Vec<u8>> = vec![
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 5 0 R] /Resources << /Font << /F1 6 0 R >> >> >>".to_vec(),
      stream(first),
      stream(second),
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
      offsets.push(pdf.len());
      pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
      pdf.extend_from_slice(body);
      pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
      pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(b"trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n");
    pdf.extend_from_slice(xref.to_string().as_bytes());
    pdf.extend_from_slice(b"\n%%EOF");
    pdf
  }

  #[test]
  fn page_keeps_content_bytes_and_segments() {
    let pdf = two_stream_pdf(b"BT /F1 12 Tf (one) Tj ET", b"BT /F1 12 Tf (two) Tj ET");
    let doc = PdfDocument::load_bytes(pdf).unwrap();
    let page = doc.page(0).unwrap();
    // The page buffer is the concatenation of both streams, and each
    // segment names the object owning that byte range.
    assert_eq!(page.content_segments[0].end, b"BT /F1 12 Tf (one) Tj ET".len());
    assert_eq!(page.content_segments.len(), 2);
    assert_eq!(page.content_segments[0].obj, 4);
    assert_eq!(page.content_segments[1].obj, 5);
    // A newline separates the concatenated streams, so the second
    // segment starts one byte after the first one ends, and the
    // trailing separator sits outside every segment.
    assert_eq!(page.content_segments[1].start, page.content_segments[0].end + 1);
    assert_eq!(page.content_segments[1].end, b"BT /F1 12 Tf (two) Tj ET".len() + page.content_segments[1].start);
    assert_eq!(page.content_segments[1].end + 1, page.content.len());
    assert_eq!(page.content[page.content_segments[0].end], b'\n');
  }

  #[test]
  fn run_spans_slice_real_page_bytes() {
    // End to end: a span taken off a parsed page must index the page's
    // own content buffer and name the stream object to rewrite.
    let pdf = two_stream_pdf(b"BT /F1 12 Tf (first) Tj ET", b"BT /F1 12 Tf 0 -20 Td (second) Tj ET");
    let doc = PdfDocument::load_bytes(pdf).unwrap();
    let page = doc.page(0).unwrap();
    assert_eq!(page.runs.len(), 2);

    let a = page.runs[0].src.expect("first run is addressable");
    assert_eq!(a.container, 4);
    assert_eq!(&page.content[a.operand_range().0..a.operand_range().1], b"(first)");

    let b = page.runs[1].src.expect("second run is addressable");
    assert_eq!(b.container, 5);
    assert_eq!(&page.content[b.operand_range().0..b.operand_range().1], b"(second)");

    // The container must agree with the segment table, otherwise a save
    // would rewrite the wrong stream.
    assert_eq!(ContentSegment::container_of(&page.content_segments, a.operand_start as usize), a.container);
    assert_eq!(ContentSegment::container_of(&page.content_segments, b.operand_start as usize), b.container);
  }

  #[test]
  fn replacing_a_run_bytes_leaves_valid_content() {
    // What M3 will do, proven here: splicing the content buffer at the
    // span and re-parsing yields the new text and nothing else.
    let pdf = two_stream_pdf(b"BT /F1 12 Tf (first) Tj ET", b"BT /F1 12 Tf (second) Tj ET");
    let doc = PdfDocument::load_bytes(pdf).unwrap();
    let page = doc.page(0).unwrap();
    let span = page.runs[0].src.expect("run is addressable");
    let (start, end) = span.operand_range();

    let mut patched = page.content.clone();
    patched.splice(start..end, b"(EDITED)".iter().copied());
    let runs = crate::page::interpret_content(&patched, &[]).expect("patched content still parses");
    assert_eq!(runs[0].text, "EDITED");
    assert_eq!(runs[1].text, "second");
  }

  #[test]
  fn tounicode_and_widths_end_to_end() {
    let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <00E6> endbfchar";
    let content = b"BT /F1 12 Tf 72 720 Td (A) Tj (B) Tj ET";
    let font = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 65 /LastChar 66 /Widths [500 600] /ToUnicode 6 0 R >>";
    let objects: Vec<Vec<u8>> = vec![
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
      [b"<< /Length ".to_vec(), content.len().to_string().into_bytes(), b" >>\nstream\n".to_vec(), content.to_vec(), b"\nendstream".to_vec()].concat(),
      font.to_vec(),
      [b"<< /Length ".to_vec(), cmap.len().to_string().into_bytes(), b" >>\nstream\n".to_vec(), cmap.to_vec(), b"\nendstream".to_vec()].concat(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
      offsets.push(pdf.len());
      pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
      pdf.extend_from_slice(body);
      pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
      pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(b"trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n");
    pdf.extend_from_slice(xref.to_string().as_bytes());
    pdf.extend_from_slice(b"\n%%EOF");
    let doc = PdfDocument::load_bytes(pdf).unwrap();
    let page = doc.page(0).unwrap();
    assert_eq!(page.runs.len(), 2);
    assert_eq!(page.runs[0].text, "æ");
    // Advance uses the real width: 500/1000 * 12pt = 6pt.
    assert_eq!(page.runs[1].x, 72.0 + 6.0);
  }

  #[test]
  fn form_xobject_recursion() {
    let form_content = b"BT /F1 10 Tf 0 0 Td (InForm) Tj ET 0 0 50 50 re f";
    let content = b"q 2 0 0 2 0 0 cm /Fm1 Do Q";
    let objects: Vec<Vec<u8>> = vec![
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> /XObject << /Fm1 5 0 R >> >> >>".to_vec(),
      [b"<< /Length ".to_vec(), content.len().to_string().into_bytes(), b" >>\nstream\n".to_vec(), content.to_vec(), b"\nendstream".to_vec()].concat(),
      [
        b"<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /Matrix [1 0 0 1 10 20] /Resources << /Font << /F1 6 0 R >> >> /Length ".to_vec(),
        form_content.len().to_string().into_bytes(),
        b" >>\nstream\n".to_vec(),
        form_content.to_vec(),
        b"\nendstream".to_vec(),
      ]
      .concat(),
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    let doc = assemble(objects);
    let page = doc.page(0).unwrap();
    assert!(page.text().contains("InForm"));
    // Form origin (10, 20) through the page scale (2x) lands at (20, 40).
    let run = page.runs.iter().find(|r| r.text == "InForm").unwrap();
    assert_eq!((run.x, run.y), (20.0, 40.0));
    assert!(page.items.iter().any(|i| matches!(i, crate::graphics::PageItem::Path(_))));
    assert!(page.items.iter().any(|i| matches!(i, crate::graphics::PageItem::Save)));
  }

  #[test]
  fn inline_image_decodes() {
    let content = b"q 10 0 0 10 100 500 cm BI /W 2 /H 1 /CS /G /BPC 1 ID \xC0 EI Q";
    let objects: Vec<Vec<u8>> = vec![
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
      [b"<< /Length ".to_vec(), content.len().to_string().into_bytes(), b" >>\nstream\n".to_vec(), content.to_vec(), b"\nendstream".to_vec()].concat(),
    ];
    let doc = assemble(objects);
    let page = doc.page(0).unwrap();
    let image = page.items.iter().find_map(|i| match i {
      crate::graphics::PageItem::Image(placed) => Some(placed),
      _ => None,
    });
    let placed = image.expect("inline image item");
    assert_eq!((placed.image.width, placed.image.height), (2, 1));
    assert_eq!(&placed.image.rgba[0..4], &[255, 255, 255, 255]);
  }

  #[test]
  fn smask_filtered_applies_once() {
    // E079 regression: the soft mask stream carries /Filter, and its
    // bytes must be decoded exactly once. Decoding twice drops the
    // mask so the image renders as if unmasked.
    use std::io::Write as _;
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&[0u8, 255]).unwrap();
    let masked = enc.finish().unwrap();
    let img = [255u8, 0, 0, 0, 0, 255];
    let content = b"q 2 0 0 1 10 10 cm /Im1 Do Q";
    let objects: Vec<Vec<u8>> = vec![
      b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>".to_vec(),
      [b"<< /Length ".to_vec(), content.len().to_string().into_bytes(), b" >>\nstream\n".to_vec(), content.to_vec(), b"\nendstream".to_vec()].concat(),
      [format!("<< /Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 6 0 R /Length {} >>\nstream\n", img.len()).into_bytes(), img.to_vec(), b"\nendstream".to_vec()].concat(),
      [format!("<< /Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n", masked.len()).into_bytes(), masked, b"\nendstream".to_vec()].concat(),
    ];
    let doc = assemble(objects);
    let placed = doc.page(0).unwrap().items.iter().find_map(|i| match i {
      crate::graphics::PageItem::Image(p) => Some(p.clone()),
      _ => None,
    }).expect("smask image item");
    assert_eq!(&placed.image.rgba[0..3], &[255, 0, 0]);
    assert_eq!(placed.image.rgba[3], 0);
    assert_eq!(&placed.image.rgba[4..7], &[0, 0, 255]);
    assert_eq!(placed.image.rgba[7], 255);
  }

  fn assemble(objects: Vec<Vec<u8>>) -> PdfDocument {
    assemble_with_info(objects, None)
  }

  fn assemble_with_info(objects: Vec<Vec<u8>>, info: Option<Vec<u8>>) -> PdfDocument {
    let mut objects = objects;
    let info_ref = info.map(|body| {
      objects.push(body);
      objects.len() as u32
    });
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
      offsets.push(pdf.len());
      pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
      pdf.extend_from_slice(body);
      pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
      pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    let mut trailer = format!("<< /Size {} /Root 1 0 R", objects.len() + 1);
    if let Some(num) = info_ref {
      trailer.push_str(&format!(" /Info {num} 0 R"));
    }
    trailer.push_str(" >>");
    pdf.extend_from_slice(b"trailer\n");
    pdf.extend_from_slice(trailer.as_bytes());
    pdf.extend_from_slice(b"\nstartxref\n");
    pdf.extend_from_slice(xref.to_string().as_bytes());
    pdf.extend_from_slice(b"\n%%EOF");
    PdfDocument::load_bytes(pdf).unwrap()
  }

  #[test]
  fn annotations_outlines_info() {
    let content = b"BT /F1 12 Tf (Body) Tj ET";
    let objects: Vec<Vec<u8>> = vec![
      b"<< /Type /Catalog /Pages 2 0 R /Outlines 8 0 R >>".to_vec(),
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R 7 0 R] >>".to_vec(),
      [b"<< /Length ".to_vec(), content.len().to_string().into_bytes(), b" >>\nstream\n".to_vec(), content.to_vec(), b"\nendstream".to_vec()].concat(),
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
      b"<< /Type /Annot /Subtype /Link /Rect [0 0 100 20] /Border [0 0 1] /A << /S /URI /URI (https://tontoo.os) >> >>".to_vec(),
      b"<< /Type /Annot /Subtype /Highlight /Rect [0 0 50 10] /C [1 1 0] /QuadPoints [0 10 50 10 50 0 0 0] >>".to_vec(),
      b"<< /First 9 0 R /Last 9 0 R /Count 1 >>".to_vec(),
      b"<< /Title (Chapter) /Parent 8 0 R /Dest [3 0 R /Fit] >>".to_vec(),
    ];
    let doc = assemble_with_info(objects, Some(b"<< /Title (Doc) /Author (Me) >>".to_vec()));
    let page = doc.page(0).unwrap();
    assert_eq!(page.annotations.len(), 2);
    let link = page.annotations.iter().find(|a| a.subtype == "Link").unwrap();
    assert_eq!(link.target, Some(crate::annot::LinkTarget::Uri("https://tontoo.os".into())));
    let mark = page.annotations.iter().find(|a| a.subtype == "Highlight").unwrap();
    assert_eq!(mark.quads.len(), 1);
    assert_eq!(doc.outlines.len(), 1);
    assert_eq!(doc.outlines[0].title, "Chapter");
    assert_eq!(doc.outlines[0].target, Some(crate::annot::LinkTarget::Page(0)));
    assert_eq!(doc.info.title.as_deref(), Some("Doc"));
    assert_eq!(doc.info.author.as_deref(), Some("Me"));
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
