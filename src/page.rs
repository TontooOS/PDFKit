use crate::error::{PdfError, Result};
use crate::parser::PageFont;

/// One positioned text run on a page.
///
/// Coordinates are PDF points with the origin at the bottom-left of
/// the page (`MediaBox`). The viewer converts them to top-left
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
}

/// A fully interpreted page: size plus positioned text runs.
///
/// Images, paths, colors and annotations are parsed in later milestones;
/// v0.1 only carries text so the viewer and editor have a real page
/// model instead of a flat bitmap.
#[derive(Debug, Clone)]
pub struct PdfPage {
  /// Zero-based page index in document order.
  pub number: usize,
  /// Page width in points.
  pub width: f32,
  /// Page height in points.
  pub height: f32,
  /// Text runs in content-stream order.
  pub runs: Vec<PdfTextRun>,
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
    let runs = interpret_content(content, fonts)?;
    Ok(Self { number, width, height, runs })
  }
}

/// Content-stream token: operands plus the operator that follows them.
#[derive(Debug, Clone)]
enum Token {
  Name(String),
  Str(Vec<u8>),
  Number(f64),
  Array(Vec<Token>),
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
    match b {
      b'(' => Ok(Some(Token::Str(self.read_literal()?))),
      b'<' => {
        if self.data.get(self.pos + 1) == Some(&b'<') {
          self.pos += 2;
          self.skip_dict()
        } else {
          Ok(Some(Token::Str(self.read_hex()?)))
        }
      }
      b'/' => {
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.data.len() && !is_content_delim(self.data[self.pos]) {
          self.pos += 1;
        }
        Ok(Some(Token::Name(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned())))
      }
      b'[' => {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
          self.skip_ws();
          match self.data.get(self.pos).copied() {
            None => return Err(PdfError::ContentParse("unterminated TJ array".into())),
            Some(b']') => {
              self.pos += 1;
              break;
            }
            Some(b'(') => items.push(Token::Str(self.read_literal()?)),
            Some(b'<') if self.data.get(self.pos + 1) != Some(&b'<') => items.push(Token::Str(self.read_hex()?)),
            Some(c) if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => items.push(Token::Number(self.read_number()?)),
            Some(_) => {
              let start = self.pos;
              while self.pos < self.data.len() && !is_content_delim(self.data[self.pos]) && self.data[self.pos] != b']' {
                self.pos += 1;
              }
              items.push(Token::Op(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned()));
            }
          }
        }
        Ok(Some(Token::Array(items)))
      }
      c if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => Ok(Some(Token::Number(self.read_number()?))),
      _ => {
        let start = self.pos;
        while self.pos < self.data.len() && !is_content_delim(self.data[self.pos]) {
          self.pos += 1;
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

  fn skip_dict(&mut self) -> Result<Option<Token>> {
    let mut depth = 1usize;
    while self.pos < self.data.len() {
      if self.data[self.pos..].starts_with(b"<<") {
        depth += 1;
        self.pos += 2;
      } else if self.data[self.pos..].starts_with(b">>") {
        depth -= 1;
        self.pos += 2;
        if depth == 0 {
          break;
        }
      } else {
        self.pos += 1;
      }
    }
    self.next_token()
  }
}

fn is_content_delim(b: u8) -> bool {
  matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'/' | b'%') || b.is_ascii_whitespace() || b == 0
}

struct TextState {
  in_text: bool,
  font: String,
  size: f32,
  char_space: f32,
  word_space: f32,
  leading: f32,
  x: f32,
  y: f32,
}

impl TextState {
  fn new() -> Self {
    Self { in_text: false, font: String::new(), size: 12.0, char_space: 0.0, word_space: 0.0, leading: 0.0, x: 0.0, y: 0.0 }
  }
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

fn font_is_bold(fonts: &[PageFont], resource: &str) -> bool {
  fonts.iter().find(|f| f.resource == resource).is_some_and(|f| f.base_font.to_lowercase().contains("bold"))
}

/// Interpret a decoded content stream into positioned text runs.
///
/// Supported operators in v0.1: `BT`, `ET`, `Tf`, `Tc`, `Tw`, `TL`,
/// `Tm`, `Td`, `TD`, `T*`, `Tj`, `TJ`, `'`, `"`. All other operators
/// (graphics, color, images) are skipped with their operands.
pub fn interpret_content(content: &[u8], fonts: &[PageFont]) -> Result<Vec<PdfTextRun>> {
  let mut lexer = Lexer::new(content);
  let mut operands: Vec<Token> = Vec::new();
  let mut state = TextState::new();
  let mut runs = Vec::new();

  while let Some(token) = lexer.next_token()? {
    match token {
      Token::Op(op) => {
        apply_operator(&op, &mut operands, &mut state, fonts, &mut runs)?;
        operands.clear();
      }
      other => operands.push(other),
    }
  }
  Ok(runs)
}

fn numbers(operands: &[Token]) -> Vec<f64> {
  operands
    .iter()
    .filter_map(|t| match t {
      Token::Number(n) => Some(*n),
      _ => None,
    })
    .collect()
}

fn apply_operator(op: &str, operands: &mut [Token], state: &mut TextState, fonts: &[PageFont], runs: &mut Vec<PdfTextRun>) -> Result<()> {
  match op {
    "BT" => {
      state.in_text = true;
      state.x = 0.0;
      state.y = 0.0;
    }
    "ET" => state.in_text = false,
    "Tf" => {
      if operands.len() >= 2 {
        if let Token::Name(name) = &operands[operands.len() - 2] {
          state.font = name.clone();
        }
        let nums = numbers(operands);
        if let Some(size) = nums.last() {
          state.size = (*size as f32).max(0.5);
        }
      }
    }
    "Tc" => {
      let nums = numbers(operands);
      if let Some(v) = nums.last() {
        state.char_space = *v as f32;
      }
    }
    "Tw" => {
      let nums = numbers(operands);
      if let Some(v) = nums.last() {
        state.word_space = *v as f32;
      }
    }
    "TL" => {
      let nums = numbers(operands);
      if let Some(v) = nums.last() {
        state.leading = *v as f32;
      }
    }
    "Tm" => {
      let nums = numbers(operands);
      if nums.len() >= 6 {
        state.x = nums[4] as f32;
        state.y = nums[5] as f32;
      }
    }
    "Td" => {
      let nums = numbers(operands);
      if nums.len() >= 2 {
        state.x += nums[nums.len() - 2] as f32;
        state.y += nums[nums.len() - 1] as f32;
      }
    }
    "TD" => {
      let nums = numbers(operands);
      if nums.len() >= 2 {
        state.x += nums[nums.len() - 2] as f32;
        state.y += nums[nums.len() - 1] as f32;
        state.leading = -(nums[nums.len() - 1] as f32);
      }
    }
    "T*" => {
      state.y -= state.leading;
    }
    "Tj" => {
      if !state.in_text {
        return Ok(());
      }
      if let Some(Token::Str(bytes)) = operands.last() {
        emit_run(state, fonts, decode_text(bytes), runs);
      }
    }
    "TJ" => {
      if !state.in_text {
        return Ok(());
      }
      if let Some(Token::Array(items)) = operands.last() {
        for item in items {
          match item {
            Token::Str(bytes) => emit_run(state, fonts, decode_text(bytes), runs),
            Token::Number(adjust) => {
              state.x -= (*adjust as f32) * state.size / 1000.0;
            }
            _ => {}
          }
        }
      }
    }
    "'" => {
      if !state.in_text {
        return Ok(());
      }
      state.y -= state.leading;
      state.x = 0.0;
      if let Some(Token::Str(bytes)) = operands.last() {
        emit_run(state, fonts, decode_text(bytes), runs);
      }
    }
    "\"" => {
      let nums = numbers(operands);
      if nums.len() >= 2 {
        state.word_space = nums[0] as f32;
        state.char_space = nums[1] as f32;
      }
      if !state.in_text {
        return Ok(());
      }
      state.y -= state.leading;
      state.x = 0.0;
      if let Some(Token::Str(bytes)) = operands.last() {
        emit_run(state, fonts, decode_text(bytes), runs);
      }
    }
    _ => {}
  }
  Ok(())
}

fn emit_run(state: &mut TextState, fonts: &[PageFont], text: String, runs: &mut Vec<PdfTextRun>) {
  if text.is_empty() {
    return;
  }
  let bold = font_is_bold(fonts, &state.font);
  let run = PdfTextRun {
    text: text.clone(),
    x: state.x,
    y: state.y,
    font_size: state.size,
    bold,
    font_name: state.font.clone(),
  };
  // Advance estimate so later runs on the same line do not overlap.
  // Exact advances need embedded font metrics (later milestone); the
  // viewer measures the real glyphs with Parley anyway.
  let glyphs = text.chars().count() as f32;
  let spaces = text.chars().filter(|c| *c == ' ').count() as f32;
  state.x += glyphs * state.size * 0.5 + glyphs * state.char_space + spaces * state.word_space;
  runs.push(run);
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
}
