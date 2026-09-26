use crate::error::{PdfError, Result};

/// Minimal PDF object model (ISO 32000, section 7.3).
///
/// Only the value types needed to walk the document catalog, the page
/// tree and stream dictionaries are represented. Streams are handled
/// separately by the parser together with their dictionary.
#[derive(Debug, Clone, PartialEq)]
pub enum PdfValue {
  Null,
  Bool(bool),
  Number(f64),
  Name(String),
  /// Literal string `(...)` as raw bytes (may carry any encoding).
  Str(Vec<u8>),
  /// Hex string `<...>` as raw bytes.
  Hex(Vec<u8>),
  Array(Vec<PdfValue>),
  /// Dictionary as an ordered list of name/value pairs.
  Dict(Vec<(String, PdfValue)>),
  /// Indirect reference `num gen R`.
  Ref(u32, u16),
}

impl PdfValue {
  /// Look up a dictionary entry by name.
  pub fn get(&self, key: &str) -> Option<&PdfValue> {
    match self {
      Self::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
      _ => None,
    }
  }

  /// Interpret the value as a number.
  pub fn as_number(&self) -> Option<f64> {
    match self {
      Self::Number(n) => Some(*n),
      _ => None,
    }
  }

  /// Interpret the value as a name.
  pub fn as_name(&self) -> Option<&str> {
    match self {
      Self::Name(n) => Some(n),
      _ => None,
    }
  }

  /// Interpret the value as an indirect reference.
  pub fn as_ref(&self) -> Option<(u32, u16)> {
    match self {
      Self::Ref(num, gen) => Some((*num, *gen)),
      _ => None,
    }
  }

  /// Interpret the value as an array.
  pub fn as_array(&self) -> Option<&[PdfValue]> {
    match self {
      Self::Array(items) => Some(items),
      _ => None,
    }
  }
}

/// Byte-level parser for indirect object bodies (dicts, arrays,
/// strings, names, numbers, references). Comments (`%...`) and
/// whitespace are skipped. Stream keywords are left to the caller.
pub struct ObjectParser<'a> {
  data: &'a [u8],
  pos: usize,
}

impl<'a> ObjectParser<'a> {
  /// Create a parser over the given slice.
  pub fn new(data: &'a [u8]) -> Self {
    Self { data, pos: 0 }
  }

  /// Current byte offset (used to slice stream bodies).
  pub fn offset(&self) -> usize {
    self.pos
  }

  /// Move the cursor to an absolute offset.
  pub fn seek(&mut self, pos: usize) {
    self.pos = pos.min(self.data.len());
  }

  /// True when all input is consumed.
  pub fn eof(&self) -> bool {
    self.pos >= self.data.len()
  }

  /// Skip whitespace and `%` comments.
  pub fn skip_ws(&mut self) {
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

  fn peek(&self) -> Option<u8> {
    self.data.get(self.pos).copied()
  }

  fn take(&mut self) -> Option<u8> {
    let b = self.data.get(self.pos).copied()?;
    self.pos += 1;
    Some(b)
  }

  fn starts_with(&self, word: &[u8]) -> bool {
    self.data[self.pos..].starts_with(word)
  }

  fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
      || b == 0
      || b == 9
      || b == 10
      || b == 12
      || b == 13
      || b == 32
  }

  /// Parse one value. Returns `Ok(None)` at end of input.
  pub fn parse_value(&mut self) -> Result<Option<PdfValue>> {
    self.skip_ws();
    let b = match self.peek() {
      None => return Ok(None),
      Some(b) => b,
    };
    let value = match b {
      b'/' => PdfValue::Name(self.parse_name()?),
      b'(' => PdfValue::Str(self.parse_literal()?),
      b'<' => {
        if self.data.get(self.pos + 1) == Some(&b'<') {
          self.parse_dict()?
        } else {
          PdfValue::Hex(self.parse_hex()?)
        }
      }
      b'[' => self.parse_array()?,
      b't' | b'f' | b'n' => self.parse_keyword()?,
      _ => self.parse_number_or_ref()?,
    };
    Ok(Some(value))
  }

  fn parse_name(&mut self) -> Result<String> {
    self.take();
    let start = self.pos;
    while let Some(b) = self.peek() {
      if Self::is_delim(b) {
        break;
      }
      self.pos += 1;
    }
    let raw = &self.data[start..self.pos];
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
      if raw[i] == b'#' && i + 2 < raw.len() {
        let hex = std::str::from_utf8(&raw[i + 1..i + 3]).unwrap_or("  ");
        if let Ok(byte) = u8::from_str_radix(hex.trim(), 16) {
          out.push(byte);
          i += 3;
          continue;
        }
      }
      out.push(raw[i]);
      i += 1;
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
  }

  fn parse_literal(&mut self) -> Result<Vec<u8>> {
    self.take();
    let mut out = Vec::new();
    let mut depth = 1usize;
    while let Some(b) = self.take() {
      match b {
        b'(' => {
          depth += 1;
          out.push(b);
        }
        b')' => {
          depth -= 1;
          if depth == 0 {
            break;
          }
          out.push(b);
        }
        b'\\' => {
          let esc = self.take().ok_or_else(|| PdfError::InvalidObject("truncated escape".into()))?;
          match esc {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'(' | b')' | b'\\' => out.push(esc),
            b'\n' => {}
            b'\r' => {
              if self.peek() == Some(b'\n') {
                self.take();
              }
            }
            d if d.is_ascii_digit() => {
              let mut code = (d - b'0') as u32;
              for _ in 0..2 {
                match self.peek() {
                  Some(next) if next.is_ascii_digit() => {
                    self.take();
                    code = code * 8 + (next - b'0') as u32;
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
    if depth != 0 {
      return Err(PdfError::InvalidObject("unterminated literal string".into()));
    }
    Ok(out)
  }

  fn parse_hex(&mut self) -> Result<Vec<u8>> {
    self.take();
    let mut digits = Vec::new();
    loop {
      self.skip_ws();
      match self.take() {
        None => return Err(PdfError::InvalidObject("unterminated hex string".into())),
        Some(b'>') => break,
        Some(b) => {
          if (b as char).is_ascii_hexdigit() {
            digits.push(b);
          } else {
            return Err(PdfError::InvalidObject("bad hex digit".into()));
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
      let byte = u8::from_str_radix(s, 16).unwrap_or(0);
      out.push(byte);
    }
    Ok(out)
  }

  fn parse_dict(&mut self) -> Result<PdfValue> {
    self.pos += 2;
    let mut entries = Vec::new();
    loop {
      self.skip_ws();
      if self.starts_with(b">>") {
        self.pos += 2;
        break;
      }
      if self.eof() {
        return Err(PdfError::InvalidObject("unterminated dictionary".into()));
      }
      if self.peek() != Some(b'/') {
        return Err(PdfError::InvalidObject("dict key must be a name".into()));
      }
      let key = self.parse_name()?;
      match self.parse_value()? {
        Some(value) => entries.push((key, value)),
        None => return Err(PdfError::InvalidObject("dict value missing".into())),
      }
    }
    Ok(PdfValue::Dict(entries))
  }

  fn parse_array(&mut self) -> Result<PdfValue> {
    self.take();
    let mut items = Vec::new();
    loop {
      self.skip_ws();
      match self.peek() {
        None => return Err(PdfError::InvalidObject("unterminated array".into())),
        Some(b']') => {
          self.take();
          break;
        }
        Some(_) => match self.parse_value()? {
          Some(value) => items.push(value),
          None => return Err(PdfError::InvalidObject("unterminated array".into())),
        },
      }
    }
    Ok(PdfValue::Array(items))
  }

  fn parse_keyword(&mut self) -> Result<PdfValue> {
    for word in ["true", "false", "null"] {
      if self.starts_with(word.as_bytes()) {
        let end = self.pos + word.len();
        if self.data.get(end).is_none_or(|b| Self::is_delim(*b)) {
          self.pos = end;
          return Ok(match word {
            "true" => PdfValue::Bool(true),
            "false" => PdfValue::Bool(false),
            _ => PdfValue::Null,
          });
        }
      }
    }
    Err(PdfError::InvalidObject("bad keyword".into()))
  }

  /// Consume an indirect object header `N G obj` and return `(N, G)`.
  /// Leaves the cursor at the start of the body value.
  pub fn read_obj_header(&mut self) -> Result<(u32, u16)> {
    let missing = || PdfError::InvalidObject("bad object header".into());
    let num_tok = self.take_token().ok_or_else(missing)?;
    let gen_tok = self.take_token().ok_or_else(missing)?;
    let keyword = self.take_token().ok_or_else(missing)?;
    if keyword != "obj" {
      return Err(PdfError::InvalidObject(format!("expected obj, found {keyword}")));
    }
    let num = num_tok.parse::<u32>().map_err(|_| missing())?;
    let gen = gen_tok.parse::<u16>().map_err(|_| missing())?;
    Ok((num, gen))
  }

  fn take_token(&mut self) -> Option<String> {
    self.skip_ws();
    let start = self.pos;
    while let Some(b) = self.peek() {
      if Self::is_delim(b) {
        break;
      }
      self.pos += 1;
    }
    if start == self.pos {
      return None;
    }
    Some(String::from_utf8_lossy(&self.data[start..self.pos]).into_owned())
  }

  fn parse_number_or_ref(&mut self) -> Result<PdfValue> {
    let checkpoint = self.pos;
    let first = self.take_token().ok_or_else(|| PdfError::InvalidObject("value missing".into()))?;
    let first_num: Option<i64> = first.parse().ok();
    let second_checkpoint = self.pos;
    let second = self.take_token();
    let third = self.take_token();
    if let (Some(num), Some(second_tok), Some(third_tok)) = (first_num, second, third) {
      if third_tok == "R" {
        if let (Ok(gen), Ok(num_u)) = (second_tok.parse::<u16>(), u32::try_from(num)) {
          let _ = second_tok.parse::<i64>();
          return Ok(PdfValue::Ref(num_u, gen));
        }
      }
    }
    self.pos = second_checkpoint;
    if let Ok(int) = first.parse::<i64>() {
      return Ok(PdfValue::Number(int as f64));
    }
    if let Ok(float) = first.parse::<f64>() {
      return Ok(PdfValue::Number(float));
    }
    self.pos = checkpoint;
    Err(PdfError::InvalidObject(format!("bad token: {first}")))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_dict_with_ref() {
    let mut p = ObjectParser::new(b"<< /Type /Page /Parent 1 0 R /MediaBox [0 0 612 792] >>");
    let value = p.parse_value().unwrap().unwrap();
    assert_eq!(value.get("Type").and_then(|v| v.as_name()), Some("Page"));
    assert_eq!(value.get("Parent").and_then(|v| v.as_ref()), Some((1, 0)));
    let box_vals = value.get("MediaBox").and_then(|v| v.as_array()).unwrap();
    assert_eq!(box_vals.len(), 4);
  }

  #[test]
  fn parses_escaped_literal() {
    let mut p = ObjectParser::new(b"(a\\(b\\) \\101)");
    let value = p.parse_value().unwrap().unwrap();
    assert_eq!(value, PdfValue::Str(b"a(b) A".to_vec()));
  }
}
