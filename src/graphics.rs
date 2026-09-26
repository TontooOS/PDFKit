use std::collections::HashMap;

use crate::color::{ResolvedPattern, ResolvedShading};
use crate::error::{PdfError, Result};
use crate::font::FontDecoder;
use crate::image::DecodedImage;
use crate::page::PdfTextRun;

/// 2D affine matrix `[a b c d e f]` in PDF row-vector convention:
/// `x' = a*x + c*y + e`, `y' = b*x + d*y + f`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
  pub a: f32,
  pub b: f32,
  pub c: f32,
  pub d: f32,
  pub e: f32,
  pub f: f32,
}

impl Matrix {
  /// Identity matrix.
  pub fn ident() -> Self {
    Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 }
  }

  /// Translation matrix.
  pub fn translate(tx: f32, ty: f32) -> Self {
    Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: tx, f: ty }
  }

  /// Concatenate: apply `self` first, then `next`
  /// (`CTM' = M x CTM` in PDF terms).
  pub fn concat(self, next: Self) -> Self {
    Self {
      a: self.a * next.a + self.b * next.c,
      b: self.a * next.b + self.b * next.d,
      c: self.c * next.a + self.d * next.c,
      d: self.c * next.b + self.d * next.d,
      e: self.e * next.a + self.f * next.c + next.e,
      f: self.e * next.b + self.f * next.d + next.f,
    }
  }

  /// Transform a point.
  pub fn apply(self, x: f32, y: f32) -> (f32, f32) {
    (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
  }
}

/// sRGB color with components in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
  pub r: f32,
  pub g: f32,
  pub b: f32,
}

impl Rgb {
  /// Black.
  pub fn black() -> Self {
    Self { r: 0.0, g: 0.0, b: 0.0 }
  }

  /// Clamp every component into range.
  pub fn clamp(self) -> Self {
    Self { r: self.r.clamp(0.0, 1.0), g: self.g.clamp(0.0, 1.0), b: self.b.clamp(0.0, 1.0) }
  }

  /// Convert to a Vello color.
  pub fn to_color(self) -> vello::peniko::Color {
    vello::peniko::Color::from_rgb8(
      (self.r.clamp(0.0, 1.0) * 255.0) as u8,
      (self.g.clamp(0.0, 1.0) * 255.0) as u8,
      (self.b.clamp(0.0, 1.0) * 255.0) as u8,
    )
  }
}

/// Naive CMYK to sRGB conversion (no undercolor removal).
pub fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> Rgb {
  Rgb {
    r: 1.0 - (c + k).min(1.0),
    g: 1.0 - (m + k).min(1.0),
    b: 1.0 - (y + k).min(1.0),
  }
}

/// Active color space for one stroking/painting side.
#[derive(Debug, Clone, PartialEq)]
pub enum ColorSpace {
  Gray,
  Rgb,
  Cmyk,
  /// Named special space (Pattern, Separation, ICCBased, ...).
  /// Handled in detail by later milestones; stored for fidelity.
  Named(String),
}

/// Fill rule for painting and clipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
  NonZero,
  EvenOdd,
}

/// Stroke attributes (PDF `w`, `J`, `j`, `M`, `d`).
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeStyle {
  pub width: f32,
  pub cap: u8,
  pub join: u8,
  pub miter: f32,
  pub dash: Vec<f32>,
  pub phase: f32,
}

impl Default for StrokeStyle {
  fn default() -> Self {
    Self { width: 1.0, cap: 0, join: 0, miter: 10.0, dash: Vec::new(), phase: 0.0 }
  }
}

/// One path segment in user space.
#[derive(Debug, Clone, PartialEq)]
pub enum PathSeg {
  Move(f32, f32),
  Line(f32, f32),
  Curve(f32, f32, f32, f32, f32, f32),
  Close,
}

/// A finished path with its paint and the CTM at paint time.
#[derive(Debug, Clone, PartialEq)]
pub struct PathItem {
  /// Subpaths; every subpath starts with `Move` (`re` closes its own).
  pub subpaths: Vec<Vec<PathSeg>>,
  /// CTM snapshot when the path was painted.
  pub ctm: Matrix,
  /// Fill paint and rule (`S` never sets this).
  pub fill: Option<(Rgb, FillRule)>,
  /// Fill alpha from `ca` (`1.0` opaque).
  pub fill_alpha: f32,
  /// Stroke paint and style.
  pub stroke: Option<(Rgb, StrokeStyle)>,
  /// Stroke alpha from `CA` (`1.0` opaque).
  pub stroke_alpha: f32,
  /// Clip rule when `W`/`W*` preceded the paint operator.
  pub clip: Option<FillRule>,
}

/// An axial or radial shading resolved for the view.
#[derive(Debug, Clone, PartialEq)]
pub struct GradientItem {
  /// CTM snapshot when the shading was painted.
  pub ctm: Matrix,
  /// Axial `(x0, y0, x1, y1)` or radial `(x0, y0, r0, x1, y1, r1)` coords.
  pub coords: Vec<f32>,
  /// True for radial, false for axial.
  pub radial: bool,
  /// Gradient stops (offset `0.0..=1.0`, sRGB).
  pub stops: Vec<(f32, Rgb)>,
  /// Extend flags beyond `[0, 1]`.
  pub extend: [bool; 2],
}

/// ExtGState parameters applied by the `gs` operator (ISO 32000 8.4.5).
/// Blend modes, overprint and soft masks parse but render as normal
/// opaque paint (documented gap); line attributes and constant alpha
/// apply fully.
#[derive(Debug, Clone, Default)]
pub struct ExtGState {
  /// Line width (`LW`).
  pub lw: Option<f32>,
  /// Line cap (`LC`).
  pub lc: Option<u8>,
  /// Line join (`LJ`).
  pub join: Option<u8>,
  /// Miter limit (`ML`).
  pub ml: Option<f32>,
  /// Dash array and phase (`D`).
  pub dash: Option<(Vec<f32>, f32)>,
  /// Nonstroking alpha (`ca`).
  pub ca: Option<f32>,
  /// Stroking alpha (`CA`).
  pub ca_stroke: Option<f32>,
  /// Blend mode name (`BM`); rendered as normal.
  pub blend: Option<String>,
}

/// Font metadata handed to the interpreter (encoding and widths
/// resolve in full; glyph outlines stay with the system fonts).
#[derive(Debug, Clone)]
pub struct FontInfo {
  /// Resource name without slash, e.g. `F1`.
  pub resource: String,
  /// `/BaseFont` name, e.g. `Helvetica-Bold`.
  pub base_font: String,
  /// True for italic/oblique faces.
  pub italic: bool,
  /// Byte-to-text/width decoder for this font.
  pub decoder: FontDecoder,
}

impl FontInfo {
  /// Plain WinAnsi font (used by tests and fallbacks).
  pub fn simple(resource: &str, base_font: &str) -> Self {
    Self {
      resource: resource.into(),
      base_font: base_font.into(),
      italic: base_font.to_lowercase().contains("italic") || base_font.to_lowercase().contains("oblique"),
      decoder: FontDecoder::winansi(),
    }
  }

  /// True when the base font name marks a bold face.
  pub fn is_bold(&self) -> bool {
    self.base_font.to_lowercase().contains("bold")
  }
}

/// Page items: the full vector/text model of a content stream.
/// Coordinates live in default user space (points, y up).
#[derive(Debug, Clone, PartialEq)]
pub enum PageItem {
  Text(PdfTextRun),
  Path(PathItem),
  Gradient(GradientItem),
  Image(PlacedImage),
  /// Pattern reference (tiling renders in a later milestone).
  Pattern(String),
  /// Skipped content with a reason (unsupported filter, depth limit).
  Skipped(String),
  /// Marked-content section start (`BMC`/`BDC` with tag).
  BeginMarked(Marked),
  /// Marked-content section end (`EMC`).
  EndMarked,
  /// Graphics state push (`q`); bounds clip lifetime for the view.
  Save,
  /// Graphics state pop (`Q`).
  Restore,
}

/// An image placed through a CTM snapshot (unit square mapping).
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedImage {
  /// Decoded RGBA pixels.
  pub image: DecodedImage,
  /// CTM mapping the unit square to user space.
  pub ctm: Matrix,
}

/// Result of resolving an XObject or inline image.
#[derive(Debug, Clone, PartialEq)]
pub enum XObjectResult {
  /// Spliced items (form content with its own Save/Restore frame).
  Items(Vec<PageItem>),
  /// A placed raster image.
  Image(PlacedImage),
  /// Skipped with a reason.
  Skipped(String),
}

/// Neutral inline-image dict value for providers.
#[derive(Debug, Clone, PartialEq)]
pub enum InlineVal {
  Name(String),
  Num(f64),
  Array(Vec<InlineVal>),
  Str(Vec<u8>),
}

impl InlineVal {
  /// Convert a content token (dicts become unsupported markers).
  fn from_token(token: &Token) -> Self {
    match token {
      Token::Name(n) => Self::Name(n.clone()),
      Token::Num(n) => Self::Num(*n),
      Token::Array(items) => Self::Array(items.iter().map(Self::from_token).collect()),
      Token::Str(bytes) => Self::Str(bytes.clone()),
      Token::Dict(_) => Self::Name(String::from("<dict>")),
      Token::InlineImage { .. } => Self::Name(String::from("<image>")),
      Token::Op(op) => Self::Name(op.clone()),
    }
  }
}

/// A marked-content section tag with optional property name.
#[derive(Debug, Clone, PartialEq)]
pub struct Marked {
  /// Tag name, e.g. `Span`, `Artifact`.
  pub tag: String,
  /// Property name (`BDC` second operand) or dict marker.
  pub prop: Option<String>,
}

/// Maximum form XObject nesting (guards cyclic forms).
pub const MAX_FORM_DEPTH: u32 = 8;

/// Resources a content stream can name. Implemented by the document
/// layer on top of `FileParser`.
pub trait ResourceProvider {
  /// Font metadata for a resource name, if declared.
  fn font(&self, name: &str) -> Option<FontInfo>;
  /// ExtGState dict for a resource name, if declared.
  fn extgstate(&self, _name: &str) -> Option<ExtGState> {
    None
  }
  /// Resolve a special color space (`Separation`, `DeviceN`,
  /// `Indexed`, `ICCBased`, calibrated) to sRGB.
  fn special_color(&self, _space: &str, _comps: &[f32]) -> Option<Rgb> {
    None
  }
  /// Resolve a shading resource for the `sh` operator.
  fn shading(&self, _name: &str) -> Option<ResolvedShading> {
    None
  }
  /// Resolve a pattern resource for `SCN`/`scn`.
  fn pattern(&self, _name: &str) -> Option<ResolvedPattern> {
    None
  }
  /// Resolve an XObject for `Do` with the current CTM, fill paint
  /// and alpha. `depth` counts form nesting (see `MAX_FORM_DEPTH`).
  fn xobject(&self, _name: &str, _ctm: Matrix, _fill: Rgb, _alpha: f32, _depth: u32) -> XObjectResult {
    XObjectResult::Skipped(String::from("no resources"))
  }
  /// Resolve an inline image (`BI..EI`) with neutral dict values.
  fn inline_image(
    &self,
    _dict: &[(String, InlineVal)],
    _data: &[u8],
    _ctm: Matrix,
    _fill: Rgb,
    _alpha: f32,
  ) -> XObjectResult {
    XObjectResult::Skipped(String::from("no resources"))
  }
}

/// Full graphics state (ISO 32000 8.4).
#[derive(Debug, Clone)]
struct State {
  ctm: Matrix,
  fill_rgb: Rgb,
  stroke_rgb: Rgb,
  fill_cs: ColorSpace,
  stroke_cs: ColorSpace,
  stroke_style: StrokeStyle,
  font: String,
  font_size: f32,
  char_space: f32,
  word_space: f32,
  h_scale: f32,
  leading: f32,
  rise: f32,
  render_mode: u8,
  fill_alpha: f32,
  stroke_alpha: f32,
  in_text: bool,
  tlm: Matrix,
}

impl State {
  fn new() -> Self {
    Self {
      ctm: Matrix::ident(),
      fill_rgb: Rgb::black(),
      stroke_rgb: Rgb::black(),
      fill_cs: ColorSpace::Gray,
      stroke_cs: ColorSpace::Gray,
      stroke_style: StrokeStyle::default(),
      font: String::new(),
      font_size: 12.0,
      char_space: 0.0,
      word_space: 0.0,
      h_scale: 1.0,
      leading: 0.0,
      rise: 0.0,
      render_mode: 0,
      fill_alpha: 1.0,
      stroke_alpha: 1.0,
      in_text: false,
      tlm: Matrix::ident(),
    }
  }

  /// Text rendering matrix origin: `([fs*h 0 0 fs 0 rise] x Tlm x CTM)(0,0)`.
  fn text_origin(&self) -> (f32, f32) {
    let font_m = Matrix {
      a: self.font_size * self.h_scale,
      b: 0.0,
      c: 0.0,
      d: self.font_size,
      e: 0.0,
      f: self.rise,
    };
    font_m.concat(self.tlm).concat(self.ctm).apply(0.0, 0.0)
  }

  /// X-axis direction of the text matrix (for future underline/selection).
  fn text_dir(&self) -> (f32, f32) {
    let font_m = Matrix {
      a: self.font_size * self.h_scale,
      b: 0.0,
      c: 0.0,
      d: self.font_size,
      e: 0.0,
      f: self.rise,
    };
    let m = font_m.concat(self.tlm).concat(self.ctm);
    let (x0, y0) = m.apply(0.0, 0.0);
    let (x1, y1) = m.apply(1.0, 0.0);
    let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt().max(1e-6);
    ((x1 - x0) / len, (y1 - y0) / len)
  }
}

/// Content-stream token: operands plus operators.
#[derive(Debug, Clone)]
enum Token {
  Name(String),
  Str(Vec<u8>),
  Num(f64),
  Array(Vec<Token>),
  /// Inline property dicts (`BDC`/`DP`); consumed by M7 structure info.
  #[allow(dead_code)]
  Dict(Vec<(String, Token)>),
  InlineImage { dict: Vec<(String, Token)>, data: Vec<u8> },
  Op(String),
}

struct Lexer<'a> {
  data: &'a [u8],
  pos: usize,
}

impl<'a> Lexer<'a> {
  fn new(data: &'a [u8]) -> Self {
    Self { data, pos: 0 }
  }

  fn skip_ws(&mut self) {
    while self.pos < self.data.len() {
      let b = self.data[self.pos];
      if b == b'%' {
        while self.pos < self.data.len() && self.data[self.pos] != b'\n' {
          self.pos += 1;
        }
      } else if b == 0 || b == 9 || b == 10 || b == 12 || b == 13 || b == 32 {
        self.pos += 1;
      } else {
        break;
      }
    }
  }

  fn next_token(&mut self) -> Result<Option<Token>> {
    self.skip_ws();
    let b = match self.data.get(self.pos).copied() {
      None => return Ok(None),
      Some(b) => b,
    };
    if b == b'B' && self.data.get(self.pos + 1) == Some(&b'I') {
      let after = self.pos + 2;
      if self.data.get(after).is_none_or(|c| c.is_ascii_whitespace() || *c == 0) {
        self.pos += 2;
        return Ok(Some(self.read_inline_image()?));
      }
    }
    match b {
      b'(' => Ok(Some(Token::Str(self.read_literal()?))),
      b'<' => {
        if self.data.get(self.pos + 1) == Some(&b'<') {
          Ok(Some(Token::Dict(self.read_dict()?)))
        } else {
          Ok(Some(Token::Str(self.read_hex()?)))
        }
      }
      b'/' => {
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.data.len() && !is_delim(self.data[self.pos]) {
          self.pos += 1;
        }
        Ok(Some(Token::Name(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned())))
      }
      b'[' => {
        self.pos += 1;
        Ok(Some(Token::Array(self.read_array()?)))
      }
      c if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => Ok(Some(Token::Num(self.read_number()?))),
      _ => {
        let start = self.pos;
        while self.pos < self.data.len() && !is_delim(self.data[self.pos]) {
          self.pos += 1;
        }
        if self.pos == start {
          // Stray delimiter (e.g. `)`, `>`, `]` from damaged content):
          // skip one byte so lexing always advances. Never emit empty
          // operators; they would spin the interpreter forever.
          self.pos += 1;
          return self.next_token();
        }
        Ok(Some(Token::Op(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned())))
      }
    }
  }

  fn read_literal(&mut self) -> Result<Vec<u8>> {
    self.pos += 1;
    let mut out = Vec::new();
    let mut depth = 1usize;
    while self.pos < self.data.len() {
      let b = self.data[self.pos];
      self.pos += 1;
      match b {
        b'(' => {
          depth += 1;
          out.push(b);
        }
        b')' => {
          depth -= 1;
          if depth == 0 {
            return Ok(out);
          }
          out.push(b);
        }
        b'\\' => {
          let esc = *self.data.get(self.pos).ok_or_else(|| PdfError::ContentParse("truncated escape".into()))?;
          self.pos += 1;
          match esc {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'(' | b')' | b'\\' => out.push(esc),
            b'\r' => {
              if self.data.get(self.pos) == Some(&b'\n') {
                self.pos += 1;
              }
            }
            b'\n' => {}
            d if d.is_ascii_digit() => {
              let mut code = (d - b'0') as u32;
              for _ in 0..2 {
                match self.data.get(self.pos) {
                  Some(n) if n.is_ascii_digit() => {
                    code = code * 8 + (n - b'0') as u32;
                    self.pos += 1;
                  }
                  _ => break,
                }
              }
              out.push((code & 0xFF) as u8);
            }
            other => {
              out.push(b'\\');
              out.push(other);
            }
          }
        }
        other => out.push(other),
      }
    }
    Err(PdfError::ContentParse("unterminated string".into()))
  }

  fn read_hex(&mut self) -> Result<Vec<u8>> {
    self.pos += 1;
    let mut digits = Vec::new();
    loop {
      self.skip_ws();
      match self.data.get(self.pos).copied() {
        None => return Err(PdfError::ContentParse("unterminated hex string".into())),
        Some(b'>') => {
          self.pos += 1;
          break;
        }
        Some(b) => {
          self.pos += 1;
          if (b as char).is_ascii_hexdigit() {
            digits.push(b);
          } else {
            return Err(PdfError::ContentParse("bad hex digit".into()));
          }
        }
      }
    }
    if digits.len() % 2 == 1 {
      digits.push(b'0');
    }
    let mut out = Vec::with_capacity(digits.len() / 2);
    for pair in digits.chunks(2) {
      let s = std::str::from_utf8(pair).unwrap_or("00");
      out.push(u8::from_str_radix(s, 16).unwrap_or(0));
    }
    Ok(out)
  }

  fn read_number(&mut self) -> Result<f64> {
    let start = self.pos;
    while self.pos < self.data.len()
      && (self.data[self.pos].is_ascii_digit() || matches!(self.data[self.pos], b'-' | b'+' | b'.' | b'e' | b'E'))
    {
      self.pos += 1;
    }
    let text = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
    text.parse::<f64>().map_err(|_| PdfError::ContentParse(format!("bad number: {text}")))
  }

  fn read_array(&mut self) -> Result<Vec<Token>> {
    let mut items = Vec::new();
    loop {
      self.skip_ws();
      match self.data.get(self.pos).copied() {
        None => return Err(PdfError::ContentParse("unterminated array".into())),
        Some(b']') => {
          self.pos += 1;
          return Ok(items);
        }
        Some(b'(') => items.push(Token::Str(self.read_literal()?)),
        Some(b'<') if self.data.get(self.pos + 1) != Some(&b'<') => items.push(Token::Str(self.read_hex()?)),
        Some(b'<') => items.push(Token::Dict(self.read_dict_at()?)),
        Some(b'/') => {
          self.pos += 1;
          let start = self.pos;
          while self.pos < self.data.len() && !is_delim(self.data[self.pos]) {
            self.pos += 1;
          }
          items.push(Token::Name(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned()));
        }
        Some(c) if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => items.push(Token::Num(self.read_number()?)),
        Some(_) => {
          let start = self.pos;
          while self.pos < self.data.len() && !is_delim(self.data[self.pos]) && self.data[self.pos] != b']' {
            self.pos += 1;
          }
          if self.pos == start {
            // Stray delimiter inside arrays: skip it, keep the array alive.
            self.pos += 1;
            continue;
          }
          items.push(Token::Op(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned()));
        }
      }
    }
  }

  fn read_dict_at(&mut self) -> Result<Vec<(String, Token)>> {
    self.pos += 2;
    let mut entries = Vec::new();
    loop {
      self.skip_ws();
      if self.data[self.pos..].starts_with(b">>") {
        self.pos += 2;
        return Ok(entries);
      }
      if self.pos >= self.data.len() {
        return Err(PdfError::ContentParse("unterminated dict".into()));
      }
      if self.data[self.pos] != b'/' {
        // Stray byte (damaged content): skip it instead of failing
        // the whole stream, unless the dict ends here.
        self.pos += 1;
        continue;
      }
      self.pos += 1;
      let start = self.pos;
      while self.pos < self.data.len() && !is_delim(self.data[self.pos]) {
        self.pos += 1;
      }
      let key = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
      self.skip_ws();
      let value = match self.data.get(self.pos).copied() {
        Some(b'(') => Token::Str(self.read_literal()?),
        Some(b'<') if self.data.get(self.pos + 1) != Some(&b'<') => Token::Str(self.read_hex()?),
        Some(b'<') => Token::Dict(self.read_dict_at()?),
        Some(b'[') => {
          self.pos += 1;
          Token::Array(self.read_array()?)
        }
        Some(b'/') => {
          self.pos += 1;
          let start = self.pos;
          while self.pos < self.data.len() && !is_delim(self.data[self.pos]) {
            self.pos += 1;
          }
          Token::Name(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned())
        }
        Some(c) if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => Token::Num(self.read_number()?),
        _ => {
          let start = self.pos;
          while self.pos < self.data.len() && !is_delim(self.data[self.pos]) {
            self.pos += 1;
          }
          Token::Op(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned())
        }
      };
      entries.push((key, value));
    }
  }

  fn read_dict(&mut self) -> Result<Vec<(String, Token)>> {
    self.read_dict_at()
  }

  /// Read an inline image: `BI` dict `ID` binary `EI`.
  /// Keys use abbreviations (`W`, `H`, `BPC`, `CS`, `F`, ...).
  fn read_inline_image(&mut self) -> Result<Token> {
    let mut dict: Vec<(String, Token)> = Vec::new();
    loop {
      self.skip_ws();
      if self.data[self.pos..].starts_with(b"ID") {
        let after = self.pos + 2;
        if self.data.get(after).is_none_or(|c| c.is_ascii_whitespace() || *c == 0) {
          self.pos += 2;
          break;
        }
      }
      if self.pos + 2 <= self.data.len() && self.data[self.pos] == b'E' && self.data[self.pos + 1] == b'I' {
        return Err(PdfError::ContentParse("ID missing in inline image".into()));
      }
      if self.pos >= self.data.len() {
        return Err(PdfError::ContentParse("truncated inline image".into()));
      }
      if self.data[self.pos] != b'/' {
        // Abbreviated keys may omit the slash in broken files; be lenient.
        let start = self.pos;
        while self.pos < self.data.len() && !is_delim(self.data[self.pos]) {
          self.pos += 1;
        }
        let key = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
        self.skip_ws();
        dict.push((inline_key(&key).into(), self.read_inline_value()?));
        continue;
      }
      self.pos += 1;
      let start = self.pos;
      while self.pos < self.data.len() && !is_delim(self.data[self.pos]) {
        self.pos += 1;
      }
      let key = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
      self.skip_ws();
      dict.push((inline_key(&key).into(), self.read_inline_value()?));
    }
    // One whitespace byte follows ID; then binary until whitespace + EI.
    if self.pos < self.data.len() && (self.data[self.pos].is_ascii_whitespace() || self.data[self.pos] == 0) {
      self.pos += 1;
    }
    let start = self.pos;
    let mut end = start;
    let mut found = false;
    let mut i = start;
    while i + 1 < self.data.len() {
      if self.data[i] == b'E'
        && self.data[i + 1] == b'I'
        && (i == 0 || self.data[i - 1].is_ascii_whitespace() || self.data[i - 1] == 0)
        && self.data.get(i + 2).is_none_or(|c| c.is_ascii_whitespace() || *c == 0)
      {
        end = i.saturating_sub(1);
        if end < start {
          end = start;
        }
        // end points at the whitespace before EI; exclude it.
        self.pos = i + 2;
        found = true;
        break;
      }
      i += 1;
    }
    if !found {
      return Err(PdfError::ContentParse("unterminated inline image".into()));
    }
    Ok(Token::InlineImage { dict, data: self.data[start..end].to_vec() })
  }

  fn read_inline_value(&mut self) -> Result<Token> {
    match self.data.get(self.pos).copied() {
      Some(b'(') => Ok(Token::Str(self.read_literal()?)),
      Some(b'<') if self.data.get(self.pos + 1) != Some(&b'<') => Ok(Token::Str(self.read_hex()?)),
      Some(b'<') => Ok(Token::Dict(self.read_dict_at()?)),
      Some(b'[') => {
        self.pos += 1;
        Ok(Token::Array(self.read_array()?))
      }
      Some(b'/') => {
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.data.len() && !is_delim(self.data[self.pos]) {
          self.pos += 1;
        }
        Ok(Token::Name(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned()))
      }
      Some(c) if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => Ok(Token::Num(self.read_number()?)),
      Some(_) => {
        // Stray byte in inline dicts: skip it.
        self.pos += 1;
        Ok(Token::Name(String::from("<skipped>")))
      }
      None => Err(PdfError::ContentParse("truncated inline image".into())),
    }
  }
}

fn is_delim(b: u8) -> bool {
  matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'/' | b'%') || b.is_ascii_whitespace() || b == 0
}

/// Expand inline-image key abbreviations to full filter-style names.
fn inline_key(key: &str) -> &str {
  match key {
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
  }
}

fn numbers(ops: &[Token]) -> Vec<f64> {
  ops.iter()
    .filter_map(|t| match t {
      Token::Num(n) => Some(*n),
      _ => None,
    })
    .collect()
}

fn token_name(token: &Token) -> Option<&str> {
  match token {
    Token::Name(n) | Token::Op(n) => Some(n),
    _ => None,
  }
}

struct Interp<R: ResourceProvider> {
  res: R,
  stack: Vec<State>,
  path: Vec<Vec<PathSeg>>,
  pending_clip: Option<FillRule>,
  compat_depth: u32,
  depth: u32,
  items: Vec<PageItem>,
}

/// Interpret a decoded content stream into page items.
pub fn interpret<R: ResourceProvider>(content: &[u8], res: R) -> Result<Vec<PageItem>> {
  interpret_with(content, res, Matrix::ident(), 0)
}

/// Interpret with a base CTM (form XObjects) and nesting depth.
pub fn interpret_with<R: ResourceProvider>(content: &[u8], res: R, base_ctm: Matrix, depth: u32) -> Result<Vec<PageItem>> {
  let mut base = State::new();
  base.ctm = base_ctm;
  let mut it = Interp { res, stack: vec![base], path: Vec::new(), pending_clip: None, compat_depth: 0, depth, items: Vec::new() };
  it.run(content)?;
  Ok(it.items)
}

impl<R: ResourceProvider> Interp<R> {
  fn state(&self) -> &State {
    self.stack.last().expect("state stack never empty")
  }

  fn state_mut(&mut self) -> &mut State {
    self.stack.last_mut().expect("state stack never empty")
  }

  fn run(&mut self, content: &[u8]) -> Result<()> {
    let mut lexer = Lexer::new(content);
    let mut ops: Vec<Token> = Vec::new();
    while let Some(token) = lexer.next_token()? {
      match token {
        Token::Op(op) => {
          if self.compat_depth > 0 {
            if op == "BX" {
              self.compat_depth += 1;
            } else if op == "EX" {
              self.compat_depth -= 1;
            }
            ops.clear();
            continue;
          }
          if op == "BX" {
            self.compat_depth = 1;
            ops.clear();
            continue;
          }
          self.apply(&op, &ops)?;
          ops.clear();
        }
        Token::InlineImage { dict, data } => {
          if self.compat_depth == 0 {
            self.inline_image(dict, data)?;
          }
          ops.clear();
        }
        other => ops.push(other),
      }
    }
    Ok(())
  }

  fn apply(&mut self, op: &str, ops: &[Token]) -> Result<()> {
    match op {
      // Graphics state.
      "q" => {
        self.items.push(PageItem::Save);
        self.stack.push(self.state().clone());
      }
      "Q" => {
        if self.stack.len() > 1 {
          self.stack.pop();
          self.items.push(PageItem::Restore);
        }
      }
      "cm" => {
        let n = numbers(ops);
        if n.len() >= 6 {
          let m = Matrix { a: n[0] as f32, b: n[1] as f32, c: n[2] as f32, d: n[3] as f32, e: n[4] as f32, f: n[5] as f32 };
          let ctm = self.state().ctm;
          self.state_mut().ctm = m.concat(ctm);
        }
      }
      "w" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().stroke_style.width = (*v as f32).max(0.0);
        }
      }
      "J" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().stroke_style.cap = (*v as u8).min(2);
        }
      }
      "j" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().stroke_style.join = (*v as u8).min(2);
        }
      }
      "M" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().stroke_style.miter = (*v as f32).max(1.0);
        }
      }
      "d" => {
        let mut dash = Vec::new();
        let mut phase = 0.0;
        if let Some(Token::Array(items)) = ops.first() {
          for item in items {
            if let Token::Num(v) = item {
              dash.push((*v as f32).max(0.0));
            }
          }
        }
        let n = numbers(ops);
        if let Some(v) = n.last() {
          phase = *v as f32;
        }
        let style = &mut self.state_mut().stroke_style;
        style.dash = dash;
        style.phase = phase;
      }
      "ri" | "i" => {}
      "gs" => {
        if let Some(name) = ops.first().and_then(token_name) {
          if let Some(gs) = self.res.extgstate(name) {
            let st = self.state_mut();
            if let Some(w) = gs.lw {
              st.stroke_style.width = w.max(0.0);
            }
            if let Some(c) = gs.lc {
              st.stroke_style.cap = c.min(2);
            }
            if let Some(j) = gs.join {
              st.stroke_style.join = j.min(2);
            }
            if let Some(m) = gs.ml {
              st.stroke_style.miter = m.max(1.0);
            }
            if let Some((dash, phase)) = gs.dash {
              st.stroke_style.dash = dash;
              st.stroke_style.phase = phase;
            }
            if let Some(a) = gs.ca {
              st.fill_alpha = a.clamp(0.0, 1.0);
            }
            if let Some(a) = gs.ca_stroke {
              st.stroke_alpha = a.clamp(0.0, 1.0);
            }
          }
        }
      }
      // Path construction.
      "m" => {
        let n = numbers(ops);
        if n.len() >= 2 {
          self.path.push(vec![PathSeg::Move(n[0] as f32, n[1] as f32)]);
        }
      }
      "l" => {
        let n = numbers(ops);
        if n.len() >= 2 {
          self.line_to(n[0] as f32, n[1] as f32);
        }
      }
      "c" => {
        let n = numbers(ops);
        if n.len() >= 6 {
          self.curve_to(n[0] as f32, n[1] as f32, n[2] as f32, n[3] as f32, n[4] as f32, n[5] as f32);
        }
      }
      "v" => {
        let n = numbers(ops);
        if n.len() >= 4 {
          let (x0, y0) = self.current_point();
          self.curve_to(x0, y0, n[0] as f32, n[1] as f32, n[2] as f32, n[3] as f32);
        }
      }
      "y" => {
        let n = numbers(ops);
        if n.len() >= 4 {
          let (x0, y0) = self.current_point();
          let _ = (x0, y0);
          let (px, py) = self.last_curve_end();
          self.curve_to(n[0] as f32, n[1] as f32, n[2] as f32, n[3] as f32, px, py);
        }
      }
      "h" => {
        if let Some(sub) = self.path.last_mut() {
          sub.push(PathSeg::Close);
        }
      }
      "re" => {
        let n = numbers(ops);
        if n.len() >= 4 {
          let (x, y, w, h) = (n[0] as f32, n[1] as f32, n[2] as f32, n[3] as f32);
          self.path.push(vec![
            PathSeg::Move(x, y),
            PathSeg::Line(x + w, y),
            PathSeg::Line(x + w, y + h),
            PathSeg::Line(x, y + h),
            PathSeg::Close,
          ]);
        }
      }
      // Path painting.
      "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => self.paint(op)?,
      "W" => self.pending_clip = Some(FillRule::NonZero),
      "W*" => self.pending_clip = Some(FillRule::EvenOdd),
      // Color.
      "G" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          let g = *v as f32;
          self.state_mut().stroke_rgb = Rgb { r: g, g, b: g };
          self.state_mut().stroke_cs = ColorSpace::Gray;
        }
      }
      "g" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          let g = *v as f32;
          self.state_mut().fill_rgb = Rgb { r: g, g, b: g };
          self.state_mut().fill_cs = ColorSpace::Gray;
        }
      }
      "RG" => {
        let n = numbers(ops);
        if n.len() >= 3 {
          let k = n.len();
          self.state_mut().stroke_rgb =
            Rgb { r: n[k - 3] as f32, g: n[k - 2] as f32, b: n[k - 1] as f32 }.clamp();
          self.state_mut().stroke_cs = ColorSpace::Rgb;
        }
      }
      "rg" => {
        let n = numbers(ops);
        if n.len() >= 3 {
          let k = n.len();
          self.state_mut().fill_rgb =
            Rgb { r: n[k - 3] as f32, g: n[k - 2] as f32, b: n[k - 1] as f32 }.clamp();
          self.state_mut().fill_cs = ColorSpace::Rgb;
        }
      }
      "K" => {
        let n = numbers(ops);
        if n.len() >= 4 {
          let k = n.len();
          self.state_mut().stroke_rgb =
            cmyk_to_rgb(n[k - 4] as f32, n[k - 3] as f32, n[k - 2] as f32, n[k - 1] as f32).clamp();
          self.state_mut().stroke_cs = ColorSpace::Cmyk;
        }
      }
      "k" => {
        let n = numbers(ops);
        if n.len() >= 4 {
          let k = n.len();
          self.state_mut().fill_rgb =
            cmyk_to_rgb(n[k - 4] as f32, n[k - 3] as f32, n[k - 2] as f32, n[k - 1] as f32).clamp();
          self.state_mut().fill_cs = ColorSpace::Cmyk;
        }
      }
      "CS" => {
        if let Some(name) = ops.first().and_then(token_name) {
          self.state_mut().stroke_cs = cs_by_name(name);
        }
      }
      "cs" => {
        if let Some(name) = ops.first().and_then(token_name) {
          self.state_mut().fill_cs = cs_by_name(name);
        }
      }
      "SC" | "SCN" => self.set_color(ops, true)?,
      "sc" | "scn" => self.set_color(ops, false)?,
      "BT" => {
        let st = self.state_mut();
        st.in_text = true;
        st.tlm = Matrix::ident();
      }
      "ET" => self.state_mut().in_text = false,
      "Tf" => {
        if ops.len() >= 2 {
          if let Some(name) = token_name(&ops[ops.len() - 2]) {
            self.state_mut().font = name.to_owned();
          }
          let n = numbers(ops);
          if let Some(size) = n.last() {
            self.state_mut().font_size = (*size as f32).max(0.1);
          }
        }
      }
      "Tc" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().char_space = *v as f32;
        }
      }
      "Tw" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().word_space = *v as f32;
        }
      }
      "Tz" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().h_scale = (*v as f32 / 100.0).clamp(0.1, 10.0);
        }
      }
      "TL" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().leading = *v as f32;
        }
      }
      "Ts" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().rise = *v as f32;
        }
      }
      "Tr" => {
        let n = numbers(ops);
        if let Some(v) = n.last() {
          self.state_mut().render_mode = (*v as u8).min(7);
        }
      }
      "Tm" => {
        let n = numbers(ops);
        if n.len() >= 6 {
          self.state_mut().tlm =
            Matrix { a: n[0] as f32, b: n[1] as f32, c: n[2] as f32, d: n[3] as f32, e: n[4] as f32, f: n[5] as f32 };
        }
      }
      "Td" => {
        let n = numbers(ops);
        if n.len() >= 2 {
          let t = Matrix::translate(n[n.len() - 2] as f32, n[n.len() - 1] as f32);
          let tlm = self.state().tlm;
          self.state_mut().tlm = t.concat(tlm);
        }
      }
      "TD" => {
        let n = numbers(ops);
        if n.len() >= 2 {
          let (tx, ty) = (n[n.len() - 2] as f32, n[n.len() - 1] as f32);
          let tlm = self.state().tlm;
          let st = self.state_mut();
          st.tlm = Matrix::translate(tx, ty).concat(tlm);
          st.leading = -ty;
        }
      }
      "T*" => {
        let leading = self.state().leading;
        let tlm = self.state().tlm;
        self.state_mut().tlm = Matrix::translate(0.0, -leading).concat(tlm);
      }
      "Tj" => {
        if self.state().in_text {
          if let Some(Token::Str(bytes)) = ops.last() {
            let bytes = bytes.clone();
            self.show(&bytes)?;
          }
        }
      }
      "TJ" => {
        if self.state().in_text {
          if let Some(Token::Array(items)) = ops.last() {
            for item in items.clone() {
              match item {
                Token::Str(bytes) => self.show(&bytes)?,
                Token::Num(adjust) => {
                  let dx = -(adjust as f32) * self.state().font_size / 1000.0 * self.state().h_scale;
                  let tlm = self.state().tlm;
                  self.state_mut().tlm = Matrix::translate(dx, 0.0).concat(tlm);
                }
                _ => {}
              }
            }
          }
        }
      }
      "'" => {
        if self.state().in_text {
          let leading = self.state().leading;
          let tlm = self.state().tlm;
          let st = self.state_mut();
          st.tlm = Matrix::translate(0.0, -leading).concat(tlm);
          st.tlm = Matrix::translate(-st.tlm.e, 0.0).concat(st.tlm);
          if let Some(Token::Str(bytes)) = ops.last() {
            let bytes = bytes.clone();
            self.show(&bytes)?;
          }
        }
      }
      "\"" => {
        let n = numbers(ops);
        if n.len() >= 2 {
          let st = self.state_mut();
          st.word_space = n[0] as f32;
          st.char_space = n[1] as f32;
        }
        if self.state().in_text {
          let leading = self.state().leading;
          let tlm = self.state().tlm;
          let st = self.state_mut();
          st.tlm = Matrix::translate(0.0, -leading).concat(tlm);
          st.tlm = Matrix::translate(-st.tlm.e, 0.0).concat(st.tlm);
          if let Some(Token::Str(bytes)) = ops.last() {
            let bytes = bytes.clone();
            self.show(&bytes)?;
          }
        }
      }
      // XObjects, patterns and shading.
      "Do" => {
        if let Some(name) = ops.first().and_then(token_name) {
          let st = self.state();
          let (ctm, fill, alpha, depth) = (st.ctm, st.fill_rgb, st.fill_alpha, self.depth);
          match self.res.xobject(name, ctm, fill, alpha, depth) {
            XObjectResult::Items(mut sub) => self.items.append(&mut sub),
            XObjectResult::Image(placed) => self.items.push(PageItem::Image(placed)),
            XObjectResult::Skipped(reason) => self.items.push(PageItem::Skipped(reason)),
          }
        }
      }
      "sh" => {
        if let Some(name) = ops.first().and_then(token_name) {
          self.paint_shading(name);
        }
      }
      // Marked content: structure markers, no rendering effect.
      "BMC" => {
        if let Some(tag) = ops.first().and_then(token_name) {
          self.items.push(PageItem::BeginMarked(Marked { tag: tag.into(), prop: None }));
        }
      }
      "BDC" => {
        if ops.len() >= 2 {
          let tag = token_name(&ops[0]).unwrap_or("").to_owned();
          let prop = match &ops[1] {
            Token::Name(n) => Some(n.clone()),
            Token::Dict(_) => Some(String::from("<dict>")),
            _ => None,
          };
          if !tag.is_empty() {
            self.items.push(PageItem::BeginMarked(Marked { tag, prop }));
          }
        }
      }
      "EMC" => self.items.push(PageItem::EndMarked),
      "MP" | "DP" => {}
      _ => {}
    }
    Ok(())
  }

  fn line_to(&mut self, x: f32, y: f32) {
    match self.path.last_mut() {
      Some(sub) => sub.push(PathSeg::Line(x, y)),
      None => self.path.push(vec![PathSeg::Move(x, y), PathSeg::Line(x, y)]),
    }
  }

  fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) {
    match self.path.last_mut() {
      Some(sub) => sub.push(PathSeg::Curve(x1, y1, x2, y2, x3, y3)),
      None => self.path.push(vec![PathSeg::Move(x1, y1), PathSeg::Curve(x1, y1, x2, y2, x3, y3)]),
    }
  }

  fn current_point(&self) -> (f32, f32) {
    match self.path.last().and_then(|sub| sub.last()) {
      Some(PathSeg::Move(x, y)) | Some(PathSeg::Line(x, y)) => (*x, *y),
      Some(PathSeg::Curve(_, _, _, _, x, y)) => (*x, *y),
      _ => (0.0, 0.0),
    }
  }

  fn last_curve_end(&self) -> (f32, f32) {
    // End point of the previous segment (control reflection base for `y`).
    // Simplified: reuse the current point (exact reflection needs the
    // second control point of the previous curve; visually close).
    self.current_point()
  }

  fn paint(&mut self, op: &str) -> Result<()> {
    let close = matches!(op, "s" | "b" | "b*");
    if close {
      if let Some(sub) = self.path.last_mut() {
        if !matches!(sub.last(), Some(PathSeg::Close)) {
          sub.push(PathSeg::Close);
        }
      }
    }
    let st = self.state();
    let fill = match op {
      "f" | "F" | "B" | "b" => Some((st.fill_rgb, FillRule::NonZero)),
      "f*" | "B*" | "b*" => Some((st.fill_rgb, FillRule::EvenOdd)),
      _ => None,
    };
    let stroke = match op {
      "S" | "s" | "B" | "B*" | "b" | "b*" => Some((st.stroke_rgb, st.stroke_style.clone())),
      _ => None,
    };
    let ctm = st.ctm;
    let fill_alpha = st.fill_alpha;
    let stroke_alpha = st.stroke_alpha;
    if op == "n" && self.pending_clip.is_none() {
      self.path.clear();
      return Ok(());
    }
    if !self.path.is_empty() || self.pending_clip.is_some() {
      self.items.push(PageItem::Path(PathItem {
        subpaths: std::mem::take(&mut self.path),
        ctm,
        fill,
        fill_alpha,
        stroke,
        stroke_alpha,
        clip: self.pending_clip,
      }));
    }
    self.pending_clip = None;
    Ok(())
  }

  fn paint_shading(&mut self, name: &str) {
    let ctm = self.state().ctm;
    let shading = self.res.shading(name);
    match shading {
      Some(ResolvedShading::Axial { coords, stops, extend }) => {
        self.items.push(PageItem::Gradient(GradientItem {
          ctm,
          coords: coords.to_vec(),
          radial: false,
          stops,
          extend,
        }));
      }
      Some(ResolvedShading::Radial { coords, stops, extend }) => {
        self.items.push(PageItem::Gradient(GradientItem {
          ctm,
          coords: coords.to_vec(),
          radial: true,
          stops,
          extend,
        }));
      }
      Some(ResolvedShading::Unsupported) | None => {}
    }
  }

  fn set_color(&mut self, ops: &[Token], stroking: bool) -> Result<()> {
    // A trailing name in SCN/scn selects a pattern; numeric operands
    // before it are the base color for uncolored tiling patterns.
    // Tiling renders later, so only the reference is kept.
    if let Some(name) = ops.last().and_then(token_name) {
      if self.res.pattern(name).is_some() {
        self.items.push(PageItem::Pattern(name.to_owned()));
        return Ok(());
      }
    }
    let st = self.state();
    let (cs, count) = if stroking {
      (st.stroke_cs.clone(), components(&st.stroke_cs))
    } else {
      (st.fill_cs.clone(), components(&st.fill_cs))
    };
    let n = numbers(ops);
    let rgb = match (&cs, count) {
      (ColorSpace::Gray, _) => {
        let g = n.last().copied().unwrap_or(0.0) as f32;
        Rgb { r: g, g, b: g }
      }
      (ColorSpace::Rgb, _) => {
        let k = n.len();
        Rgb {
          r: n.get(k.saturating_sub(3)).copied().unwrap_or(0.0) as f32,
          g: n.get(k.saturating_sub(2)).copied().unwrap_or(0.0) as f32,
          b: n.get(k.saturating_sub(1)).copied().unwrap_or(0.0) as f32,
        }
      }
      (ColorSpace::Cmyk, _) => {
        let k = n.len();
        cmyk_to_rgb(
          n.get(k.saturating_sub(4)).copied().unwrap_or(0.0) as f32,
          n.get(k.saturating_sub(3)).copied().unwrap_or(0.0) as f32,
          n.get(k.saturating_sub(2)).copied().unwrap_or(0.0) as f32,
          n.get(k.saturating_sub(1)).copied().unwrap_or(0.0) as f32,
        )
      }
      (ColorSpace::Named(space), _) => {
        let comps: Vec<f32> = n.iter().map(|v| *v as f32).collect();
        self.res.special_color(&space, &comps).unwrap_or_else(|| {
          if stroking {
            self.state().stroke_rgb
          } else {
            self.state().fill_rgb
          }
        })
      }
    }
    .clamp();
    let st = self.state_mut();
    if stroking {
      st.stroke_rgb = rgb;
    } else {
      st.fill_rgb = rgb;
    }
    Ok(())
  }

  fn show(&mut self, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || !self.state().in_text {
      return Ok(());
    }
    // Render mode 3 is invisible text (common for OCR layers).
    if self.state().render_mode == 3 {
      return Ok(());
    }
    let st = self.state();
    let decoder = self.res.font(&st.font).map(|f| f.decoder).unwrap_or_else(FontDecoder::winansi);
    let codes = decoder.codes(bytes);
    let text: String = codes.iter().map(|c| decoder.text_of(*c)).collect();
    if text.is_empty() {
      return Ok(());
    }
    let st = self.state();
    let (x, y) = st.text_origin();
    let (dx, dy) = st.text_dir();
    let bold = self.res.font(&st.font).is_some_and(|f| f.is_bold());
    let color = st.fill_rgb;
    let (size, h_scale, char_space, word_space) = (st.font_size, st.h_scale, st.char_space, st.word_space);
    let font_name = st.font.clone();
    let alpha = st.fill_alpha;
    self.items.push(PageItem::Text(PdfTextRun {
      text: text.clone(),
      x,
      y,
      font_size: size,
      bold,
      font_name,
      color_rgb: [color.r, color.g, color.b],
      dir_x: dx,
      dir_y: dy,
      alpha,
    }));
    // Advance from real glyph widths when known (1/1000 em),
    // plus char/word spacing; falls back to half-em per code.
    let widths: f32 = codes.iter().map(|c| decoder.width_of(*c)).sum::<f32>() / 1000.0;
    let spaces = text.chars().filter(|c| *c == ' ').count() as f32;
    let advance = (widths * size + codes.len() as f32 * char_space + spaces * word_space) * h_scale;
    let tlm = self.state().tlm;
    self.state_mut().tlm = Matrix::translate(advance, 0.0).concat(tlm);
    Ok(())
  }

  fn inline_image(&mut self, dict: Vec<(String, Token)>, data: Vec<u8>) -> Result<()> {
    let st = self.state();
    let (ctm, fill, alpha) = (st.ctm, st.fill_rgb, st.fill_alpha);
    let neutral: Vec<(String, InlineVal)> = dict.into_iter().map(|(k, v)| (k, InlineVal::from_token(&v))).collect();
    match self.res.inline_image(&neutral, &data, ctm, fill, alpha) {
      XObjectResult::Items(mut sub) => self.items.append(&mut sub),
      XObjectResult::Image(placed) => self.items.push(PageItem::Image(placed)),
      XObjectResult::Skipped(reason) => self.items.push(PageItem::Skipped(reason)),
    }
    Ok(())
  }
}

fn cs_by_name(name: &str) -> ColorSpace {
  match name {
    "DeviceGray" | "G" => ColorSpace::Gray,
    "DeviceRGB" | "RGB" => ColorSpace::Rgb,
    "DeviceCMYK" | "CMYK" => ColorSpace::Cmyk,
    other => ColorSpace::Named(other.into()),
  }
}

fn components(cs: &ColorSpace) -> usize {
  match cs {
    ColorSpace::Gray => 1,
    ColorSpace::Rgb => 3,
    ColorSpace::Cmyk => 4,
    ColorSpace::Named(_) => 0,
  }
}

/// Empty provider for unit tests without a document.
#[derive(Debug, Clone, Copy)]
pub struct NoResources;

impl ResourceProvider for NoResources {
  fn font(&self, _name: &str) -> Option<FontInfo> {
    None
  }
}

/// Map-based provider for unit tests.
#[derive(Debug, Clone, Default)]
pub struct MapResources {
  /// Resource name to font info.
  pub fonts: HashMap<String, FontInfo>,
}

impl ResourceProvider for MapResources {
  fn font(&self, name: &str) -> Option<FontInfo> {
    self.fonts.get(name).cloned()
  }
}

/// Collect the text runs of an item list in order.
pub fn text_runs(items: &[PageItem]) -> Vec<PdfTextRun> {
  items
    .iter()
    .filter_map(|item| match item {
      PageItem::Text(run) => Some(run.clone()),
      _ => None,
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn provider() -> MapResources {
    MapResources {
      fonts: [
        ("F1".into(), FontInfo::simple("F1", "Helvetica")),
        ("F2".into(), FontInfo::simple("F2", "Helvetica-Bold")),
      ]
      .into_iter()
      .collect(),
    }
  }

  fn texts(items: &[PageItem]) -> Vec<&PdfTextRun> {
    items
      .iter()
      .filter_map(|item| match item {
        PageItem::Text(run) => Some(run),
        _ => None,
      })
      .collect()
  }

  #[test]
  fn paints_rgb_rectangle() {
    let items = interpret(b"1 0 0 rg 10 20 30 40 re f", provider()).unwrap();
    assert_eq!(items.len(), 1);
    match &items[0] {
      PageItem::Path(p) => {
        assert_eq!(p.fill, Some((Rgb { r: 1.0, g: 0.0, b: 0.0 }, FillRule::NonZero)));
        assert_eq!(p.stroke, None);
        assert_eq!(p.subpaths.len(), 1);
      }
      other => panic!("expected path, found {other:?}"),
    }
  }

  #[test]
  fn strokes_with_dash_and_gray() {
    let items = interpret(b"0.5 G 2 w [3 2] 1 d 0 0 m 10 10 l S", provider()).unwrap();
    match &items[0] {
      PageItem::Path(p) => {
        let (rgb, style) = p.stroke.clone().unwrap();
        assert_eq!(rgb, Rgb { r: 0.5, g: 0.5, b: 0.5 });
        assert_eq!(style.width, 2.0);
        assert_eq!(style.dash, vec![3.0, 2.0]);
        assert_eq!(style.phase, 1.0);
      }
      other => panic!("expected path, found {other:?}"),
    }
  }

  #[test]
  fn clips_with_even_odd() {
    let items = interpret(b"0 0 10 10 re W* n", provider()).unwrap();
    match &items[0] {
      PageItem::Path(p) => {
        assert_eq!(p.clip, Some(FillRule::EvenOdd));
        assert_eq!(p.fill, None);
        assert_eq!(p.stroke, None);
      }
      other => panic!("expected path, found {other:?}"),
    }
  }

  #[test]
  fn cmyk_converts_to_rgb() {
    let items = interpret(b"0 1 1 0 k 0 0 1 1 re f", provider()).unwrap();
    match &items[0] {
      PageItem::Path(p) => {
        assert_eq!(p.fill, Some((Rgb { r: 1.0, g: 0.0, b: 0.0 }, FillRule::NonZero)));
      }
      other => panic!("expected path, found {other:?}"),
    }
  }

  #[test]
  fn text_uses_tm_and_fill_color() {
    let items = interpret(b"BT /F2 10 Tf 0 0 1 rg 1 0 0 1 50 600 Tm (Hi) Tj ET", provider()).unwrap();
    let runs = texts(&items);
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].x, runs[0].y), (50.0, 600.0));
    assert!(runs[0].bold);
    assert_eq!(runs[0].color_rgb, [0.0, 0.0, 1.0]);
  }

  #[test]
  fn cm_scales_text_position() {
    let items = interpret(b"2 0 0 2 0 0 cm BT /F1 12 Tf 10 10 Td (A) Tj ET", provider()).unwrap();
    let runs = texts(&items);
    assert_eq!((runs[0].x, runs[0].y), (20.0, 20.0));
  }

  #[test]
  fn compat_section_ignored() {
    let items = interpret(b"BX /Foo 1 2 3 QFunky EX BT /F1 12 Tf (ok) Tj ET", provider()).unwrap();
    let runs = texts(&items);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "ok");
  }

  #[test]
  fn save_restore_marked() {
    let items = interpret(b"q 1 0 0 RG 0 0 m 1 1 l S Q", provider()).unwrap();
    assert!(matches!(items[0], PageItem::Save));
    assert!(matches!(items[2], PageItem::Restore));
  }

  #[test]
  fn stray_delimiters_terminate() {
    // Damaged content must never spin the lexer: stray bytes are
    // skipped and the valid tail still interprets.
    let items = interpret(b") ] >> \x00 BT /F1 12 Tf (ok) Tj ET ]", provider()).unwrap();
    let runs: Vec<_> = items
      .iter()
      .filter_map(|item| match item {
        PageItem::Text(run) => Some(run),
        _ => None,
      })
      .collect();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "ok");
  }

  #[test]
  fn stray_inside_array_terminates() {
    let items = interpret(b"BT /F1 12 Tf [(a) ) > (b)] TJ ET", provider()).unwrap();
    let runs: Vec<_> = items
      .iter()
      .filter_map(|item| match item {
        PageItem::Text(run) => Some(run),
        _ => None,
      })
      .collect();
    assert_eq!(runs.len(), 2);
  }
}
