use crate::graphics::Rgb;
use crate::objects::PdfValue;

/// Link target of a link annotation, outline item or action.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
  /// Zero-based page index with fit kind preserved as text.
  Page(usize),
  /// External URI.
  Uri(String),
  /// Named destination (unresolved at parse time).
  Named(String),
}

/// A page annotation (ISO 32000 12.5). Only rendering-relevant
/// subtypes carry geometry; widgets and media annotations parse to
/// `Other` so the editor can handle them later.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
  /// Appearance rectangle in user space.
  pub rect: [f32; 4],
  /// Subtype name, e.g. `Link`, `Highlight`, `Text`.
  pub subtype: String,
  /// Optional human text (`/Contents`).
  pub contents: Option<String>,
  /// Annotation color (`/C`).
  pub color: Option<Rgb>,
  /// Border width (`/Border` or `/BS`); `0.0` hides the border.
  pub border_width: f32,
  /// Markup quads (`/QuadPoints`, 8 numbers each).
  pub quads: Vec<[f32; 8]>,
  /// Ink strokes (`/InkList`, point polylines).
  pub ink: Vec<Vec<(f32, f32)>>,
  /// Link target for `Link` annotations.
  pub target: Option<LinkTarget>,
}

/// A bookmark outline item with resolved page targets.
#[derive(Debug, Clone, PartialEq)]
pub struct Outline {
  /// Bookmark title.
  pub title: String,
  /// Resolved target when the destination points into this document.
  pub target: Option<LinkTarget>,
  /// Child bookmarks.
  pub children: Vec<Outline>,
}

/// Document metadata from the trailer `/Info` dict.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocInfo {
  /// `/Title`.
  pub title: Option<String>,
  /// `/Author`.
  pub author: Option<String>,
  /// `/Subject`.
  pub subject: Option<String>,
  /// `/Keywords`.
  pub keywords: Option<String>,
  /// `/Creator`.
  pub creator: Option<String>,
  /// `/Producer`.
  pub producer: Option<String>,
  /// `/CreationDate` (raw PDF date string).
  pub creation_date: Option<String>,
  /// `/ModDate` (raw PDF date string).
  pub mod_date: Option<String>,
}

/// Decode a PDF text string: UTF-16BE with BOM or PDFDocEncoding.
pub fn pdfdoc_to_string(bytes: &[u8]) -> String {
  if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
    let units: Vec<u16> = bytes[2..].chunks(2).map(|c| ((c[0] as u16) << 8) | *c.get(1).unwrap_or(&0) as u16).collect();
    return String::from_utf16_lossy(&units);
  }
  bytes.iter().map(|&b| pdfdoc_char(b)).collect()
}

fn pdfdoc_char(b: u8) -> char {
  match b {
    0x18 => '˘',
    0x19 => 'ˇ',
    0x1A => 'ˆ',
    0x1B => '˙',
    0x1C => '˝',
    0x1D => '˛',
    0x1E => '˜',
    0x7F | 0x80..=0xA0 => '\u{FFFD}',
    _ => b as char,
  }
}

fn rect_of(value: &PdfValue) -> Option<[f32; 4]> {
  let items = value.as_array()?;
  if items.len() < 4 {
    return None;
  }
  Some([
    items[0].as_number().unwrap_or(0.0) as f32,
    items[1].as_number().unwrap_or(0.0) as f32,
    items[2].as_number().unwrap_or(0.0) as f32,
    items[3].as_number().unwrap_or(0.0) as f32,
  ])
}

fn array_color(value: &PdfValue) -> Option<Rgb> {
  let items = value.as_array()?;
  let n: Vec<f32> = items.iter().filter_map(|v| v.as_number().map(|x| x as f32)).collect();
  match n.len() {
    1 => Some(Rgb { r: n[0], g: n[0], b: n[0] }),
    3 => Some(Rgb { r: n[0], g: n[1], b: n[2] }),
    4 => Some(crate::graphics::cmyk_to_rgb(n[0], n[1], n[2], n[3])),
    _ => None,
  }
}

/// Parse one resolved annotation dict. `resolve_page` maps an
/// explicit destination to a page index (see document layer).
pub fn parse_annotation(dict: &PdfValue, resolve_page: &dyn Fn(&PdfValue) -> Option<usize>) -> Option<Annotation> {
  let subtype = dict.get("Subtype").and_then(|v| v.as_name()).unwrap_or("").to_owned();
  if subtype.is_empty() {
    return None;
  }
  let rect = dict.get("Rect").and_then(rect_of).unwrap_or([0.0, 0.0, 0.0, 0.0]);
  let contents = match dict.get("Contents") {
    Some(PdfValue::Str(bytes)) | Some(PdfValue::Hex(bytes)) => Some(pdfdoc_to_string(bytes)),
    _ => None,
  };
  let color = dict.get("C").and_then(array_color);
  let mut border_width = 1.0;
  if let Some(items) = dict.get("Border").and_then(|v| v.as_array()) {
    // `/Border [h v w]` or `[h v w dash]`; the width is the third entry.
    if items.len() >= 3 {
      border_width = items[2].as_number().unwrap_or(1.0) as f32;
    } else {
      border_width = 0.0;
    }
  }
  if let Some(bs) = dict.get("BS") {
    if let PdfValue::Dict(_) = bs {
      border_width = bs.get("W").and_then(|v| v.as_number()).unwrap_or(1.0) as f32;
    }
  }
  let mut quads = Vec::new();
  if let Some(items) = dict.get("QuadPoints").and_then(|v| v.as_array()) {
    for chunk in items.chunks(8) {
      if chunk.len() == 8 {
        let mut quad = [0.0; 8];
        for (i, slot) in quad.iter_mut().enumerate() {
          *slot = chunk[i].as_number().unwrap_or(0.0) as f32;
        }
        quads.push(quad);
      }
    }
  }
  let mut ink = Vec::new();
  if let Some(strokes) = dict.get("InkList").and_then(|v| v.as_array()) {
    for stroke in strokes {
      if let Some(points) = stroke.as_array() {
        let mut line = Vec::new();
        for pair in points.chunks(2) {
          if pair.len() == 2 {
            line.push((pair[0].as_number().unwrap_or(0.0) as f32, pair[1].as_number().unwrap_or(0.0) as f32));
          }
        }
        if !line.is_empty() {
          ink.push(line);
        }
      }
    }
  }
  let target = if subtype == "Link" {
    link_target(dict, resolve_page)
  } else {
    None
  };
  Some(Annotation { rect, subtype, contents, color, border_width, quads, ink, target })
}

fn link_target(dict: &PdfValue, resolve_page: &dyn Fn(&PdfValue) -> Option<usize>) -> Option<LinkTarget> {
  if let Some(dest) = dict.get("Dest") {
    return dest_target(dest, resolve_page);
  }
  let action = dict.get("A")?;
  let kind = action.get("S").and_then(|v| v.as_name()).unwrap_or("");
  match kind {
    "GoTo" => action.get("D").and_then(|d| dest_target(d, resolve_page)),
    "URI" => match action.get("URI") {
      Some(PdfValue::Str(bytes)) | Some(PdfValue::Hex(bytes)) => {
        Some(LinkTarget::Uri(String::from_utf8_lossy(bytes).into_owned()))
      }
      _ => None,
    },
    "Named" => match action.get("N") {
      Some(PdfValue::Name(n)) => Some(LinkTarget::Named(n.clone())),
      _ => None,
    },
    _ => None,
  }
}

fn dest_target(dest: &PdfValue, resolve_page: &dyn Fn(&PdfValue) -> Option<usize>) -> Option<LinkTarget> {
  match dest {
    PdfValue::Name(n) => {
      if let Ok(page) = n.parse::<usize>() {
        Some(LinkTarget::Page(page))
      } else {
        Some(LinkTarget::Named(n.clone()))
      }
    }
    PdfValue::Str(n) => {
      if let Ok(page) = String::from_utf8_lossy(n).parse::<usize>() {
        Some(LinkTarget::Page(page))
      } else {
        Some(LinkTarget::Named(String::from_utf8_lossy(n).into_owned()))
      }
    }
    PdfValue::Hex(bytes) => Some(LinkTarget::Named(pdfdoc_to_string(bytes))),
    PdfValue::Array(items) => {
      if let Some(first) = items.first() {
        if let Some(page) = resolve_page(first) {
          return Some(LinkTarget::Page(page));
        }
        if let Some(n) = first.as_number() {
          return Some(LinkTarget::Page(n as usize));
        }
      }
      None
    }
    _ => None,
  }
}

/// Parse the outline tree from a resolved `/Outlines` dict value.
pub fn parse_outlines(
  value: &PdfValue,
  child: &dyn Fn(&PdfValue) -> Option<PdfValue>,
  resolve_page: &dyn Fn(&PdfValue) -> Option<usize>,
) -> Vec<Outline> {
  let first = value.get("First").and_then(|v| child(v));
  let mut out = Vec::new();
  let mut current = first;
  while let Some(item) = current {
    let title = match item.get("Title") {
      Some(PdfValue::Str(bytes)) | Some(PdfValue::Hex(bytes)) => pdfdoc_to_string(bytes),
      _ => String::new(),
    };
    let target = item
      .get("Dest")
      .and_then(|d| dest_target(d, resolve_page))
      .or_else(|| item.get("A").and_then(|a| link_target(a, resolve_page)));
    let children = match item.get("First") {
      Some(next) => match child(next) {
        Some(first_child) => {
          let mut nested = Vec::new();
          let mut cursor = Some(first_child);
          while let Some(entry) = cursor {
            nested.push(parse_outline_item(&entry, child, resolve_page));
            cursor = entry.get("Next").and_then(|v| child(v));
          }
          nested
        }
        None => vec![],
      },
      None => vec![],
    };
    out.push(Outline { title, target, children });
    current = item.get("Next").and_then(|v| child(v));
  }
  out
}

fn parse_outline_item(
  item: &PdfValue,
  child: &dyn Fn(&PdfValue) -> Option<PdfValue>,
  resolve_page: &dyn Fn(&PdfValue) -> Option<usize>,
) -> Outline {
  let title = match item.get("Title") {
    Some(PdfValue::Str(bytes)) | Some(PdfValue::Hex(bytes)) => pdfdoc_to_string(bytes),
    _ => String::new(),
  };
  let target = item
    .get("Dest")
    .and_then(|d| dest_target(d, resolve_page))
    .or_else(|| item.get("A").and_then(|a| link_target(a, resolve_page)));
  let mut children = Vec::new();
  if let Some(first) = item.get("First").and_then(|v| child(v)) {
    let mut cursor = Some(first);
    while let Some(entry) = cursor {
      children.push(parse_outline_item(&entry, child, resolve_page));
      cursor = entry.get("Next").and_then(|v| child(v));
    }
  }
  Outline { title, target, children }
}

/// Parse the trailer `/Info` dict into metadata.
pub fn parse_info(dict: &PdfValue) -> DocInfo {
  let text = |key: &str| match dict.get(key) {
    Some(PdfValue::Str(bytes)) | Some(PdfValue::Hex(bytes)) => Some(pdfdoc_to_string(bytes)),
    _ => None,
  };
  DocInfo {
    title: text("Title"),
    author: text("Author"),
    subject: text("Subject"),
    keywords: text("Keywords"),
    creator: text("Creator"),
    producer: text("Producer"),
    creation_date: text("CreationDate"),
    mod_date: text("ModDate"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn dict(pairs: Vec<(&str, PdfValue)>) -> PdfValue {
    PdfValue::Dict(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
  }

  fn num(n: f64) -> PdfValue {
    PdfValue::Number(n)
  }

  #[test]
  fn link_with_uri_action() {
    let annot = dict(vec![
      ("Subtype".into(), PdfValue::Name("Link".into())),
      ("Rect".into(), PdfValue::Array(vec![num(0.0), num(0.0), num(10.0), num(10.0)])),
      (
        "A".into(),
        dict(vec![
          ("S".into(), PdfValue::Name("URI".into())),
          ("URI".into(), PdfValue::Str(b"https://example.com".to_vec())),
        ]),
      ),
    ]);
    let parsed = parse_annotation(&annot, &|_| None).unwrap();
    assert_eq!(parsed.target, Some(LinkTarget::Uri("https://example.com".into())));
    assert_eq!(parsed.rect, [0.0, 0.0, 10.0, 10.0]);
  }

  #[test]
  fn highlight_quads_and_color() {
    let annot = dict(vec![
      ("Subtype".into(), PdfValue::Name("Highlight".into())),
      ("Rect".into(), PdfValue::Array(vec![num(0.0), num(0.0), num(20.0), num(10.0)])),
      ("C".into(), PdfValue::Array(vec![num(1.0), num(1.0), num(0.0)])),
      (
        "QuadPoints".into(),
        PdfValue::Array(vec![num(0.0), num(0.0), num(20.0), num(0.0), num(20.0), num(10.0), num(0.0), num(10.0)]),
      ),
    ]);
    let parsed = parse_annotation(&annot, &|_| None).unwrap();
    assert_eq!(parsed.quads.len(), 1);
    assert_eq!(parsed.color, Some(Rgb { r: 1.0, g: 1.0, b: 0.0 }));
  }

  #[test]
  fn goto_resolves_page_ref() {
    let annot = dict(vec![
      ("Subtype".into(), PdfValue::Name("Link".into())),
      ("Dest".into(), PdfValue::Array(vec![PdfValue::Ref(7, 0), PdfValue::Name("Fit".into())])),
    ]);
    let parsed = parse_annotation(&annot, &|v| match v {
      PdfValue::Ref(7, _) => Some(2),
      _ => None,
    })
    .unwrap();
    assert_eq!(parsed.target, Some(LinkTarget::Page(2)));
  }

  #[test]
  fn outlines_nest() {
    let child = dict(vec![("Title".into(), PdfValue::Str(b"Kid".to_vec()))]);
    let root = dict(vec![
      ("Title".into(), PdfValue::Str(b"Root".to_vec())),
      ("First".into(), PdfValue::Ref(9, 0)),
    ]);
    let outlines = dict(vec![("First".into(), PdfValue::Ref(8, 0))]);
    let by_ref = |v: &PdfValue| match v {
      PdfValue::Ref(8, _) => Some(root.clone()),
      PdfValue::Ref(9, _) => Some(child.clone()),
      _ => None,
    };
    let items = parse_outlines(&outlines, &by_ref, &|_| None);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Root");
    assert_eq!(items[0].children.len(), 1);
    assert_eq!(items[0].children[0].title, "Kid");
  }

  #[test]
  fn info_and_pdfdoc() {
    assert_eq!(pdfdoc_to_string(b"Hi"), "Hi");
    let info = dict(vec![("Title".into(), PdfValue::Str(b"Report".to_vec()))]);
    assert_eq!(parse_info(&info).title.as_deref(), Some("Report"));
  }

  #[test]
  fn three_part_border_takes_width() {
    // `/Border [h v w]`: the width is the third entry.
    let annot = dict(vec![
      ("Subtype".into(), PdfValue::Name("Link".into())),
      ("Rect".into(), PdfValue::Array(vec![num(0.0), num(0.0), num(10.0), num(10.0)])),
      (
        "Border".into(),
        PdfValue::Array(vec![num(0.0), num(0.0), num(2.0)]),
      ),
    ]);
    let parsed = parse_annotation(&annot, &|_| None).unwrap();
    assert_eq!(parsed.border_width, 2.0);
    let hidden = dict(vec![
      ("Subtype".into(), PdfValue::Name("Link".into())),
      ("Border".into(), PdfValue::Array(vec![num(0.0), num(0.0), num(0.0)])),
    ]);
    assert_eq!(parse_annotation(&hidden, &|_| None).unwrap().border_width, 0.0);
  }
}
