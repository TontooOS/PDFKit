use std::collections::HashMap;

use crate::error::Result;

/// A ToUnicode CMap: codespace ranges plus bfchar/bfrange mappings.
#[derive(Debug, Clone, Default)]
pub struct CMap {
  /// `(lo, hi, byte length)` codespace ranges.
  pub ranges: Vec<(u32, u32, usize)>,
  /// Single-code mappings to Unicode text.
  pub singles: HashMap<u32, String>,
  /// Range mappings `(lo, hi, destinations)`.
  pub ranges_map: Vec<(u32, u32, RangeDst)>,
}

/// Destination of a `bfrange` entry.
#[derive(Debug, Clone)]
pub enum RangeDst {
  /// Base string; code offset added to the last UTF-16 unit.
  Base(String),
  /// One string per code in the range.
  Array(Vec<String>),
}

impl CMap {
  /// Split bytes into codes following the codespace ranges
  /// (longest prefix match, 1-byte fallback).
  pub fn codes(&self, bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
      let mut matched = None;
      for len in (1..=4usize).rev() {
        if i + len > bytes.len() {
          continue;
        }
        let mut code = 0u32;
        for b in &bytes[i..i + len] {
          code = (code << 8) | u32::from(*b);
        }
        if self.ranges.iter().any(|(lo, hi, n)| *n == len && code >= *lo && code <= *hi) {
          matched = Some((code, len));
          break;
        }
      }
      match matched {
        Some((code, len)) => {
          out.push(code);
          i += len;
        }
        None => {
          out.push(u32::from(bytes[i]));
          i += 1;
        }
      }
    }
    out
  }

  /// Map one code to Unicode text (replacement char when unmapped).
  pub fn text_of(&self, code: u32) -> String {
    if let Some(text) = self.singles.get(&code) {
      return text.clone();
    }
    for (lo, hi, dst) in &self.ranges_map {
      if code >= *lo && code <= *hi {
        let offset = code - lo;
        return match dst {
          RangeDst::Base(base) => add_to_last_unit(base, offset),
          RangeDst::Array(items) => items.get(offset as usize).cloned().unwrap_or_else(|| "\u{FFFD}".into()),
        };
      }
    }
    "\u{FFFD}".into()
  }
}

fn add_to_last_unit(base: &str, offset: u32) -> String {
  let mut units: Vec<u16> = base.encode_utf16().collect();
  if let Some(last) = units.last_mut() {
    *last = last.wrapping_add(offset as u16);
  }
  String::from_utf16_lossy(&units)
}

/// Decode a ToUnicode CMap stream (ASCII with `<hex>` strings).
pub fn parse_cmap(data: &[u8]) -> Result<CMap> {
  let text = String::from_utf8_lossy(data);
  let tokens = tokenize_cmap(&text);
  let mut cmap = CMap::default();
  // Counts precede their keyword (`2 beginbfchar`).
  let count_before = |i: usize| -> usize {
    if i == 0 {
      return 0;
    }
    tokens[i - 1].parse().unwrap_or(0)
  };
  let mut i = 0;
  while i < tokens.len() {
    match tokens[i].as_str() {
      "begincodespacerange" => {
        let count = count_before(i);
        let mut j = i + 1;
        for _ in 0..count {
          if j + 1 >= tokens.len() {
            break;
          }
          let (lo, hi) = (hex_u32(&tokens[j]), hex_u32(&tokens[j + 1]));
          let nbytes = tokens[j].trim_start_matches('<').trim_end_matches('>').len() / 2;
          cmap.ranges.push((lo, hi, nbytes.max(1)));
          j += 2;
        }
        i = j;
      }
      "beginbfchar" => {
        let count = count_before(i);
        let mut j = i + 1;
        for _ in 0..count {
          if j + 1 >= tokens.len() {
            break;
          }
          cmap.singles.insert(hex_u32(&tokens[j]), utf16be_to_string(&tokens[j + 1]));
          j += 2;
        }
        i = j;
      }
      "beginbfrange" => {
        let count = count_before(i);
        let mut j = i + 1;
        for _ in 0..count {
          if j + 2 >= tokens.len() {
            break;
          }
          let (lo, hi) = (hex_u32(&tokens[j]), hex_u32(&tokens[j + 1]));
          if tokens[j + 2] == "[" {
            let mut items = Vec::new();
            let mut k = j + 3;
            while k < tokens.len() && tokens[k] != "]" {
              items.push(utf16be_to_string(&tokens[k]));
              k += 1;
            }
            cmap.ranges_map.push((lo, hi, RangeDst::Array(items)));
            j = k + 1;
          } else {
            cmap.ranges_map.push((lo, hi, RangeDst::Base(utf16be_to_string(&tokens[j + 2]))));
            j += 3;
          }
        }
        i = j;
      }
      _ => i += 1,
    }
  }
  Ok(cmap)
}

fn tokenize_cmap(text: &str) -> Vec<String> {
  let mut tokens = Vec::new();
  let bytes = text.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    let b = bytes[i];
    if b.is_ascii_whitespace() {
      i += 1;
    } else if b == b'%' {
      while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
      }
    } else if b == b'<' {
      let start = i;
      i += 1;
      while i < bytes.len() && bytes[i] != b'>' {
        i += 1;
      }
      i += 1;
      tokens.push(text[start..i.min(text.len())].into());
    } else if b == b'[' || b == b']' {
      tokens.push((b as char).to_string());
      i += 1;
    } else if b == b')' || b == b'>' || b == b'}' {
      // Stray bytes (damaged streams): skip so tokenizing advances.
      i += 1;
    } else {
      let start = i;
      while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !b"<>[]()%".contains(&bytes[i]) {
        i += 1;
      }
      if i == start {
        i += 1;
        continue;
      }
      tokens.push(text[start..i].into());
    }
  }
  tokens
}

fn hex_u32(token: &str) -> u32 {
  let hex = token.trim_start_matches('<').trim_end_matches('>');
  u32::from_str_radix(hex, 16).unwrap_or(0)
}

fn utf16be_to_string(token: &str) -> String {
  let hex = token.trim_start_matches('<').trim_end_matches('>');
  let mut digits = hex.to_owned();
  if digits.len() % 2 == 1 {
    digits.push('0');
  }
  let bytes: Vec<u8> = (0..digits.len())
    .step_by(2)
    .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap_or(0))
    .collect();
  let units: Vec<u16> = bytes.chunks(2).map(|c| ((c[0] as u16) << 8) | *c.get(1).unwrap_or(&0) as u16).collect();
  String::from_utf16_lossy(&units)
}

/// How bytes map to text and widths for one font.
#[derive(Debug, Clone)]
pub enum DecoderKind {
  /// Simple font: byte to char table.
  Simple([char; 256]),
  /// ToUnicode CMap (simple and CID fonts).
  CMap(CMap),
  /// Identity CID without ToUnicode (gaps documented).
  Identity,
}

/// Byte-to-text/width decoder for one font.
#[derive(Debug, Clone)]
pub struct FontDecoder {
  /// Decoding strategy.
  pub kind: DecoderKind,
  /// Glyph widths in 1/1000 em by code.
  pub widths: HashMap<u32, f32>,
  /// Width used when a code has no entry.
  pub default_width: f32,
}

impl FontDecoder {
  /// WinAnsi simple decoder with default metrics.
  pub fn winansi() -> Self {
    Self { kind: DecoderKind::Simple(winansi_table()), widths: HashMap::new(), default_width: 500.0 }
  }

  /// Simple decoder for a base encoding name.
  pub fn simple_named(encoding: &str) -> Self {
    let kind = DecoderKind::Simple(match encoding {
      "MacRomanEncoding" => macroman_table(),
      "StandardEncoding" => standard_table(),
      _ => winansi_table(),
    });
    Self { kind, widths: HashMap::new(), default_width: 500.0 }
  }

  /// Split bytes into codes.
  pub fn codes(&self, bytes: &[u8]) -> Vec<u32> {
    match &self.kind {
      DecoderKind::Simple(_) => bytes.iter().map(|b| u32::from(*b)).collect(),
      DecoderKind::CMap(cmap) => cmap.codes(bytes),
      DecoderKind::Identity => bytes.chunks(2).map(|c| ((c[0] as u32) << 8) | *c.get(1).unwrap_or(&0) as u32).collect(),
    }
  }

  /// Map one code to text.
  pub fn text_of(&self, code: u32) -> String {
    match &self.kind {
      DecoderKind::Simple(table) => {
        let ch = table.get(code as usize).copied().unwrap_or('\u{FFFD}');
        if ch == '\0' {
          String::new()
        } else {
          ch.to_string()
        }
      }
      DecoderKind::CMap(cmap) => cmap.text_of(code),
      DecoderKind::Identity => "\u{FFFD}".into(),
    }
  }

  /// Decode whole bytes to text.
  pub fn decode(&self, bytes: &[u8]) -> String {
    self.codes(bytes).iter().map(|c| self.text_of(*c)).collect()
  }

  /// Width of one code in 1/1000 em.
  pub fn width_of(&self, code: u32) -> f32 {
    self.widths.get(&code).copied().unwrap_or(self.default_width)
  }
}

/// Apply `/Differences` overrides (code to glyph name) to a table.
pub fn apply_differences(table: &mut [char; 256], diffs: &[(u32, String)]) {
  for (code, name) in diffs {
    if let Some(slot) = table.get_mut(*code as usize) {
      *slot = glyph_name_to_char(name).unwrap_or('\0');
    }
  }
}

/// Resolve an Adobe glyph name to a char (single letters plus a
/// curated subset; unknown names yield `None`).
pub fn glyph_name_to_char(name: &str) -> Option<char> {
  if name.len() == 1 {
    return name.chars().next();
  }
  Some(match name {
    "space" => ' ',
    "exclam" => '!',
    "quotedbl" => '"',
    "numbersign" => '#',
    "dollar" => '$',
    "percent" => '%',
    "ampersand" => '&',
    "quotesingle" => '\'',
    "parenleft" => '(',
    "parenright" => ')',
    "asterisk" => '*',
    "plus" => '+',
    "comma" => ',',
    "hyphen" => '-',
    "period" => '.',
    "slash" => '/',
    "colon" => ':',
    "semicolon" => ';',
    "less" => '<',
    "equal" => '=',
    "greater" => '>',
    "question" => '?',
    "at" => '@',
    "bracketleft" => '[',
    "backslash" => '\\',
    "bracketright" => ']',
    "asciicircum" => '^',
    "underscore" => '_',
    "grave" => '`',
    "braceleft" => '{',
    "bar" => '|',
    "braceright" => '}',
    "asciitilde" => '~',
    "exclamdown" => '¡',
    "cent" => '¢',
    "sterling" => '£',
    "fraction" => '⁄',
    "yen" => '¥',
    "florin" => 'ƒ',
    "section" => '§',
    "currency" => '¤',
    "quotedblleft" => '“',
    "guillemotleft" => '«',
    "guilsinglleft" => '‹',
    "guilsinglright" => '›',
    "fi" => 'ﬁ',
    "fl" => 'ﬂ',
    "endash" => '–',
    "dagger" => '†',
    "daggerdbl" => '‡',
    "periodcentered" => '·',
    "paragraph" => '¶',
    "bullet" => '•',
    "quotesinglbase" => '‚',
    "quotedblbase" => '„',
    "quotedblright" => '”',
    "guillemotright" => '»',
    "ellipsis" => '…',
    "perthousand" => '‰',
    "questiondown" => '¿',
    "circumflex" => 'ˆ',
    "tilde" => '˜',
    "macron" => '¯',
    "breve" => '˘',
    "dotaccent" => '˙',
    "dieresis" => '¨',
    "ring" => '˚',
    "cedilla" => '¸',
    "hungarumlaut" => '˝',
    "ogonek" => '˛',
    "caron" => 'ˇ',
    "emdash" => '—',
    "AE" => 'Æ',
    "ordfeminine" => 'ª',
    "Lslash" => 'Ł',
    "Oslash" => 'Ø',
    "OE" => 'Œ',
    "ordmasculine" => 'º',
    "ae" => 'æ',
    "dotlessi" => 'ı',
    "lslash" => 'ł',
    "oslash" => 'ø',
    "oe" => 'œ',
    "germandbls" => 'ß',
    "Aacute" => 'Á',
    "Acircumflex" => 'Â',
    "Adieresis" => 'Ä',
    "Agrave" => 'À',
    "Aring" => 'Å',
    "Atilde" => 'Ã',
    "Ccedilla" => 'Ç',
    "Eacute" => 'É',
    "Ecircumflex" => 'Ê',
    "Edieresis" => 'Ë',
    "Egrave" => 'È',
    "Iacute" => 'Í',
    "Icircumflex" => 'Î',
    "Idieresis" => 'Ï',
    "Igrave" => 'Ì',
    "Ntilde" => 'Ñ',
    "Oacute" => 'Ó',
    "Ocircumflex" => 'Ô',
    "Odieresis" => 'Ö',
    "Ograve" => 'Ò',
    "Otilde" => 'Õ',
    "Uacute" => 'Ú',
    "Ucircumflex" => 'Û',
    "Udieresis" => 'Ü',
    "Ugrave" => 'Ù',
    "Yacute" => 'Ý',
    "Ydieresis" => 'Ÿ',
    "aacute" => 'á',
    "acircumflex" => 'â',
    "adieresis" => 'ä',
    "agrave" => 'à',
    "aring" => 'å',
    "atilde" => 'ã',
    "ccedilla" => 'ç',
    "eacute" => 'é',
    "ecircumflex" => 'ê',
    "edieresis" => 'ë',
    "egrave" => 'è',
    "iacute" => 'í',
    "icircumflex" => 'î',
    "idieresis" => 'ï',
    "igrave" => 'ì',
    "ntilde" => 'ñ',
    "oacute" => 'ó',
    "ocircumflex" => 'ô',
    "odieresis" => 'ö',
    "ograve" => 'ò',
    "otilde" => 'õ',
    "uacute" => 'ú',
    "ucircumflex" => 'û',
    "udieresis" => 'ü',
    "ugrave" => 'ù',
    "yacute" => 'ý',
    "ydieresis" => 'ÿ',
    "Euro" => '€',
    "trademark" => '™',
    "registered" => '®',
    "copyright" => '©',
    "degree" => '°',
    "plusminus" => '±',
    "multiply" => '×',
    "divide" => '÷',
    "minus" => '−',
    "onesuperior" => '¹',
    "twosuperior" => '²',
    "threesuperior" => '³',
    "onehalf" => '½',
    "onequarter" => '¼',
    "threequarters" => '¾',
    "ffi" => '\u{FB03}',
    "ffl" => '\u{FB04}',
    _ => return None,
  })
}

fn winansi_table() -> [char; 256] {
  let mut table = ['\0'; 256];
  for (i, slot) in table.iter_mut().enumerate() {
    *slot = winansi_byte(i as u8);
  }
  table
}

fn winansi_byte(b: u8) -> char {
  if b < 0x80 {
    b as char
  } else {
    match b {
      0x80 => '€',
      0x82 => '‚',
      0x83 => 'ƒ',
      0x84 => '„',
      0x85 => '…',
      0x86 => '†',
      0x87 => '‡',
      0x88 => 'ˆ',
      0x89 => '‰',
      0x8A => 'Š',
      0x8B => '‹',
      0x8C => 'Œ',
      0x8E => 'Ž',
      0x91 => '‘',
      0x92 => '’',
      0x93 => '“',
      0x94 => '”',
      0x95 => '•',
      0x96 => '–',
      0x97 => '—',
      0x98 => '˜',
      0x99 => '™',
      0x9A => 'š',
      0x9B => '›',
      0x9C => 'œ',
      0x9E => 'ž',
      0x9F => 'Ÿ',
      _ => b as char,
    }
  }
}

fn standard_table() -> [char; 256] {
  let mut table = ['\0'; 256];
  for b in 0x20..=0x7Eu8 {
    table[b as usize] = b as char;
  }
  // High half transcribed from the Adobe StandardEncoding vector.
  let named: &[(u8, &str)] = &[
    (161, "exclamdown"),
    (162, "cent"),
    (163, "sterling"),
    (164, "fraction"),
    (165, "yen"),
    (166, "florin"),
    (167, "section"),
    (168, "currency"),
    (169, "quotesingle"),
    (170, "quotedblleft"),
    (171, "guillemotleft"),
    (172, "guilsinglleft"),
    (173, "guilsinglright"),
    (174, "fi"),
    (175, "fl"),
    (177, "endash"),
    (178, "dagger"),
    (179, "daggerdbl"),
    (180, "periodcentered"),
    (182, "paragraph"),
    (183, "bullet"),
    (184, "quotesinglbase"),
    (185, "quotedblbase"),
    (186, "quotedblright"),
    (187, "guillemotright"),
    (188, "ellipsis"),
    (189, "perthousand"),
    (191, "questiondown"),
    (192, "grave"),
    (193, "acute"),
    (194, "circumflex"),
    (195, "tilde"),
    (196, "macron"),
    (197, "breve"),
    (198, "dotaccent"),
    (199, "dieresis"),
    (200, "ring"),
    (201, "cedilla"),
    (202, "hungarumlaut"),
    (203, "ogonek"),
    (204, "caron"),
    (205, "emdash"),
    (224, "AE"),
    (225, "ordfeminine"),
    (226, "Lslash"),
    (227, "Oslash"),
    (228, "OE"),
    (229, "ordmasculine"),
    (230, "ae"),
    (231, "dotlessi"),
    (232, "lslash"),
    (233, "oslash"),
    (234, "oe"),
    (235, "germandbls"),
  ];
  for (code, name) in named {
    table[*code as usize] = glyph_name_to_char(name).unwrap_or('\0');
  }
  table
}

fn macroman_table() -> [char; 256] {
  let mut table = ['\0'; 256];
  for b in 0..=0x7Fu8 {
    table[b as usize] = b as char;
  }
  let high: &[char] = &[
    'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', 'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è', //
    'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ', 'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü', //
    '†', '°', '¢', '£', '§', '•', '¶', 'ß', '®', '©', '™', '´', '¨', '≠', 'Æ', 'Ø', //
    '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø', //
    '¿', '¡', '¬', '√', 'ƒ', '≈', '∆', '«', '»', '…', ' ', 'À', 'Ã', 'Õ', 'Œ', 'œ', //
    '–', '—', '“', '”', '‘', '’', '÷', '◊', 'ÿ', 'Ÿ', '⁄', '¤', '‹', '›', 'ﬁ', 'ﬂ', //
    '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', 'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô', //
    '\u{F8FF}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ', //
  ];
  for (i, ch) in high.iter().enumerate() {
    table[0x80 + i] = *ch;
  }
  table
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn bfchar_maps_codes() {
    let cmap = parse_cmap(b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <0042> endbfchar").unwrap();
    assert_eq!(cmap.codes(&[0x41]), vec![0x41]);
    assert_eq!(cmap.text_of(0x41), "B");
    assert_eq!(cmap.text_of(0x42), "\u{FFFD}");
  }

  #[test]
  fn bfrange_with_base_and_array() {
    let data = b"1 begincodespacerange <0000> <FFFF> endcodespacerange 2 beginbfrange <0041> <0043> <0061> <0068> <0069> [<006B> <006C>] endbfrange";
    let cmap = parse_cmap(data).unwrap();
    assert_eq!(cmap.codes(&[0x00, 0x42]), vec![0x42]);
    assert_eq!(cmap.text_of(0x41), "a");
    assert_eq!(cmap.text_of(0x43), "c");
    assert_eq!(cmap.text_of(0x68), "k");
    assert_eq!(cmap.text_of(0x69), "l");
  }

  #[test]
  fn surrogate_pair_decodes() {
    let cmap = parse_cmap(b"1 beginbfchar <41> <D83DDE00> endbfchar").unwrap();
    assert_eq!(cmap.text_of(0x41), "😀");
  }

  #[test]
  fn differences_override_winansi() {
    let mut table = winansi_table();
    apply_differences(&mut table, &[(0x41, "ae".into())]);
    assert_eq!(table[0x41], 'æ');
    assert_eq!(table[0x42], 'B');
  }

  #[test]
  fn macroman_spot_checks() {
    let table = macroman_table();
    assert_eq!(table[0x80], 'Ä');
    assert_eq!(table[0xA0], '†');
    assert_eq!(table[0xD2], '“');
    assert_eq!(table[0x41], 'A');
  }

  #[test]
  fn standard_spot_checks() {
    let table = standard_table();
    assert_eq!(table[0x41], 'A');
    assert_eq!(table[174], 'ﬁ');
    assert_eq!(table[205], '—');
    assert_eq!(table[235], 'ß');
  }

  #[test]
  fn stray_bytes_terminate_cmap() {
    // Damaged streams must not spin the tokenizer into an
    // allocation loop; valid entries still parse.
    let cmap = parse_cmap(b")>]>\x00 1 beginbfchar <41> <0042> endbfchar ]").unwrap();
    assert_eq!(cmap.text_of(0x41), "B");
  }

  #[test]
  fn glyph_names_cover_latin() {
    assert_eq!(glyph_name_to_char("eacute"), Some('é'));
    assert_eq!(glyph_name_to_char("A"), Some('A'));
    assert_eq!(glyph_name_to_char("nonsense"), None);
  }
}
