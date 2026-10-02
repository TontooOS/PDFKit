use crate::error::{PdfError, Result};
use crate::objects::PdfValue;

/// Hard cap for any single decoded stream (512 MiB). Legitimate
/// streams stay far below; decompression bombs and corrupt length
/// prefixes end here instead of in the OOM killer.
pub const MAX_STREAM_BYTES: usize = 512 * 1024 * 1024;

/// Compress bytes as a zlib stream for `/Filter /FlateDecode`.
///
/// Used by the incremental update writer, which is the only place that
/// emits new stream data: parsing never needs an encoder.
pub fn deflate(data: &[u8]) -> Vec<u8> {
  use std::io::Write as _;
  let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(data).expect("writing to a Vec cannot fail");
  encoder.finish().expect("ZlibEncoder::finish cannot fail for in-memory output")
}

/// Decode a stream body honoring `/Filter` and `/DecodeParms`.
///
/// Supported in v0.1: `FlateDecode` (with PNG/TIFF predictors),
/// `LZWDecode` (with `EarlyChange`), `ASCIIHexDecode`,
/// `ASCII85Decode`, `RunLengthDecode`. `DCTDecode` and `JPXDecode`
/// pass their bytes through (already compressed image data).
/// `CCITTFaxDecode` and `JBIG2Decode` return `UnsupportedFilter`.
pub fn decode(dict: &PdfValue, raw: &[u8]) -> Result<Vec<u8>> {
  let filters = filter_list(dict)?;
  let parms = parms_list(dict, filters.len())?;
  let mut bytes = raw.to_vec();
  for (filter, filter_parms) in filters.iter().zip(parms.iter()) {
    bytes = match filter.as_str() {
      "FlateDecode" | "Fl" => {
        let decoded = inflate(&bytes)?;
        apply_predictor(&decoded, filter_parms)?
      }
      "LZWDecode" | "LZW" => {
        let early = filter_parms.get("EarlyChange").and_then(|v| v.as_number()).unwrap_or(1.0) as i64;
        let decoded = lzw_decode(&bytes, early)?;
        apply_predictor(&decoded, filter_parms)?
      }
      "ASCIIHexDecode" | "AHx" => ascii_hex_decode(&bytes)?,
      "ASCII85Decode" | "A85" => ascii85_decode(&bytes)?,
      "RunLengthDecode" | "RL" => runlength_decode(&bytes)?,
      "DCTDecode" | "DCT" | "JPXDecode" => bytes,
      "CCITTFaxDecode" | "CCF" | "JBIG2Decode" => {
        return Err(PdfError::UnsupportedFilter(filter.clone()));
      }
      other => return Err(PdfError::UnsupportedFilter(other.to_owned())),
    };
  }
  Ok(bytes)
}

fn filter_list(dict: &PdfValue) -> Result<Vec<String>> {
  match dict.get("Filter") {
    None | Some(PdfValue::Null) => Ok(vec![]),
    Some(PdfValue::Name(name)) => Ok(vec![name.clone()]),
    Some(PdfValue::Array(items)) => {
      let mut out = Vec::new();
      for item in items {
        match item.as_name() {
          Some(name) => out.push(name.to_owned()),
          None => return Err(PdfError::InvalidObject("bad /Filter entry".into())),
        }
      }
      Ok(out)
    }
    _ => Err(PdfError::InvalidObject("bad /Filter entry".into())),
  }
}

fn parms_list(dict: &PdfValue, count: usize) -> Result<Vec<PdfValue>> {
  match dict.get("DecodeParms") {
    None | Some(PdfValue::Null) => Ok(vec![PdfValue::Null; count]),
    Some(single @ PdfValue::Dict(_)) => {
      if count == 1 {
        Ok(vec![single.clone()])
      } else {
        Err(PdfError::InvalidObject("single /DecodeParms for multiple filters".into()))
      }
    }
    Some(PdfValue::Array(items)) => {
      if items.len() != count {
        return Err(PdfError::InvalidObject("bad /DecodeParms array".into()));
      }
      Ok(items.clone())
    }
    _ => Err(PdfError::InvalidObject("bad /DecodeParms entry".into())),
  }
}

fn inflate(raw: &[u8]) -> Result<Vec<u8>> {
  use std::io::Read;
  let decoder = flate2::read::ZlibDecoder::new(raw);
  let mut limited = decoder.take(MAX_STREAM_BYTES as u64 + 1);
  let mut out = Vec::new();
  limited.read_to_end(&mut out).map_err(|e| PdfError::StreamDecode(e.to_string()))?;
  if out.len() > MAX_STREAM_BYTES {
    return Err(PdfError::StreamDecode("stream exceeds size cap".into()));
  }
  Ok(out)
}

/// Apply a predictor from `/DecodeParms` (`/Predictor 1` means none).
/// Handles PNG optimum filters (10-15) and TIFF horizontal (2).
pub fn apply_predictor(data: &[u8], parms: &PdfValue) -> Result<Vec<u8>> {
  let predictor = parms.get("Predictor").and_then(|v| v.as_number()).unwrap_or(1.0) as i64;
  if predictor == 1 || data.is_empty() {
    return Ok(data.to_vec());
  }
  let colors = parms.get("Colors").and_then(|v| v.as_number()).unwrap_or(1.0) as usize;
  let bits = parms.get("BitsPerComponent").and_then(|v| v.as_number()).unwrap_or(8.0) as usize;
  let columns = parms.get("Columns").and_then(|v| v.as_number()).unwrap_or(1.0) as usize;
  if colors == 0 || bits == 0 || columns == 0 {
    return Err(PdfError::StreamDecode("bad predictor parameters".into()));
  }
  if predictor == 2 {
    if bits != 8 {
      return Err(PdfError::StreamDecode("TIFF predictor needs 8 bpc".into()));
    }
    let row = colors * columns;
    if data.len() % row != 0 {
      return Err(PdfError::StreamDecode("bad row length for TIFF predictor".into()));
    }
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(row) {
      for (i, &b) in chunk.iter().enumerate() {
        let prev = if i >= colors { out[out.len() - colors] } else { 0 };
        out.push(b.wrapping_add(prev));
      }
    }
    return Ok(out);
  }
  let row_bytes = (colors * columns * bits + 7) / 8;
  // A row can never exceed the remaining input; corrupt DecodeParms
  // (or hostile floats) end here instead of allocating gigabytes.
  if row_bytes == 0 || row_bytes > data.len() {
    return Err(PdfError::StreamDecode("bad predictor row length".into()));
  }
  let stride = colors * ((bits + 7) / 8).max(1);
  let mut out = Vec::new();
  let mut pos = 0;
  let mut prev: Vec<u8> = vec![0; row_bytes];
  while pos < data.len() {
    if pos + 1 + row_bytes > data.len() {
      return Err(PdfError::StreamDecode("truncated predictor row".into()));
    }
    let tag = data[pos];
    pos += 1;
    let row = &data[pos..pos + row_bytes];
    pos += row_bytes;
    let filter = if predictor >= 10 { tag } else { 2 };
    if filter > 4 {
      return Err(PdfError::StreamDecode(format!("bad PNG filter {filter}")));
    }
    let mut cur = vec![0u8; row_bytes];
    for i in 0..row_bytes {
      let a = if i >= stride { cur[i - stride] } else { 0 };
      let b = prev[i];
      let c = if i >= stride { prev[i - stride] } else { 0 };
      let pred = match filter {
        0 => 0,
        1 => a,
        2 => b,
        3 => ((a as u16 + b as u16) / 2) as u8,
        _ => paeth(a, b, c),
      };
      cur[i] = row[i].wrapping_add(pred);
    }
    out.extend_from_slice(&cur);
    prev = cur;
  }
  Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
  let (a, b, c) = (a as i16, b as i16, c as i16);
  let p = a + b - c;
  let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
  if pa <= pb && pa <= pc {
    a as u8
  } else if pb <= pc {
    b as u8
  } else {
    c as u8
  }
}

/// Decode ASCIIHex (`00`-`FF` pairs, `>` ends the data).
pub fn ascii_hex_decode(data: &[u8]) -> Result<Vec<u8>> {
  let mut digits = Vec::new();
  for &b in data {
    if b == b'>' {
      break;
    }
    if b.is_ascii_whitespace() {
      continue;
    }
    if !(b as char).is_ascii_hexdigit() {
      return Err(PdfError::StreamDecode("bad ASCIIHex digit".into()));
    }
    digits.push(b);
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

/// Decode ASCII85 (`z` for zero quad, `~>` ends the data).
pub fn ascii85_decode(data: &[u8]) -> Result<Vec<u8>> {
  let mut out = Vec::new();
  let mut group: Vec<u32> = Vec::with_capacity(5);
  let mut i = 0;
  let stop = data.len();
  if data.starts_with(b"<~") {
    i = 2;
  }
  while i < stop {
    let b = data[i];
    i += 1;
    if b.is_ascii_whitespace() {
      continue;
    }
    if b == b'z' {
      if !group.is_empty() {
        return Err(PdfError::StreamDecode("stray z in ASCII85".into()));
      }
      out.extend_from_slice(&[0, 0, 0, 0]);
      continue;
    }
    if b == b'~' {
      break;
    }
    if !(b'!'..=b'u').contains(&b) {
      return Err(PdfError::StreamDecode("bad ASCII85 digit".into()));
    }
    group.push((b - b'!') as u32);
    if group.len() == 5 {
      let mut value = 0u32;
      for d in &group {
        value = value * 85 + d;
      }
      out.extend_from_slice(&value.to_be_bytes());
      group.clear();
    }
  }
  let _ = stop;
  if !group.is_empty() {
    if group.len() == 1 {
      return Err(PdfError::StreamDecode("bad final ASCII85 group".into()));
    }
    let n = group.len();
    while group.len() < 5 {
      group.push(84);
    }
    let mut value = 0u32;
    for d in &group {
      value = value * 85 + d;
    }
    out.extend_from_slice(&value.to_be_bytes()[..n - 1]);
  }
  Ok(out)
}

/// Decode RunLength (`0x80` ends, `<128` literal run, `>128` repeat).
pub fn runlength_decode(data: &[u8]) -> Result<Vec<u8>> {
  let mut out = Vec::new();
  let mut i = 0;
  while i < data.len() {
    if out.len() > MAX_STREAM_BYTES {
      return Err(PdfError::StreamDecode("stream exceeds size cap".into()));
    }
    let len = data[i] as usize;
    i += 1;
    if len == 128 {
      break;
    } else if len < 128 {
      let count = len + 1;
      if i + count > data.len() {
        return Err(PdfError::StreamDecode("truncated RunLength data".into()));
      }
      out.extend_from_slice(&data[i..i + count]);
      i += count;
    } else {
      let count = 257 - len;
      let b = *data.get(i).ok_or_else(|| PdfError::StreamDecode("truncated RunLength repeat".into()))?;
      i += 1;
      out.extend(std::iter::repeat(b).take(count));
    }
  }
  Ok(out)
}

struct BitReader<'a> {
  data: &'a [u8],
  pos: usize,
  width: u32,
}

impl<'a> BitReader<'a> {
  fn new(data: &'a [u8]) -> Self {
    Self { data, pos: 0, width: 9 }
  }

  fn read(&mut self) -> Option<u32> {
    let mut code = 0u32;
    for _ in 0..self.width {
      let byte = *self.data.get(self.pos / 8)?;
      let bit = (byte >> (7 - (self.pos % 8))) & 1;
      code = (code << 1) | bit as u32;
      self.pos += 1;
    }
    Some(code)
  }
}

/// Decode LZW with PDF code assignment (256 clear, 257 EOD).
/// `early_change` selects the width-growth rule (1 is the default).
pub fn lzw_decode(data: &[u8], early_change: i64) -> Result<Vec<u8>> {
  const CLEAR: u32 = 256;
  const EOD: u32 = 257;
  // Index equals code: 0-255 literals plus dummies for EOD/CLEAR.
  let mut table: Vec<Vec<u8>> = (0u32..256).map(|b| vec![b as u8]).collect();
  table.push(Vec::new());
  table.push(Vec::new());
  let mut next_code = 258u32;
  let mut reader = BitReader::new(data);
  let mut out = Vec::new();
  let mut prev: Option<Vec<u8>> = None;
  loop {
    let code = reader.read().ok_or_else(|| PdfError::StreamDecode("truncated LZW data".into()))?;
    if code == EOD {
      break;
    }
    if code == CLEAR {
      table.truncate(258);
      next_code = 258;
      reader.width = 9;
      prev = None;
      continue;
    }
    let entry = if (code as usize) < table.len() {
      table[code as usize].clone()
    } else if code == next_code {
      match &prev {
        Some(seq) => {
          let mut seq = seq.clone();
          seq.push(seq[0]);
          seq
        }
        None => return Err(PdfError::StreamDecode("bad LZW code".into())),
      }
    } else {
      return Err(PdfError::StreamDecode(format!("bad LZW code {code}")));
    };
    out.extend_from_slice(&entry);
    if out.len() > MAX_STREAM_BYTES {
      return Err(PdfError::StreamDecode("stream exceeds size cap".into()));
    }
    if let Some(prev_seq) = prev {
      if next_code <= 4096 {
        let mut new_entry = prev_seq;
        new_entry.push(entry[0]);
        table.push(new_entry);
        next_code += 1;
        // The decoder learns each pair one code later than the encoder
        // assigns it, so it must widen its read width one code before
        // the encoder widened its output (`EarlyChange` compensation):
        // early=1 switches at 510/1022/2046, early=0 at 511/1023/2047.
        let limit = (1u32 << reader.width) - 1 - u32::from(early_change != 0);
        if next_code == limit && reader.width < 12 {
          reader.width += 1;
        }
      }
    }
    prev = Some(entry);
  }
  Ok(out)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;

  fn dict_of(filter: &str) -> PdfValue {
    PdfValue::Dict(vec![("Filter".into(), PdfValue::Name(filter.into()))])
  }

  #[test]
  fn ascii_hex_roundtrip() {
    let raw = b"48656C6C6F>";
    assert_eq!(decode(&dict_of("ASCIIHexDecode"), raw).unwrap(), b"Hello");
  }

  #[test]
  fn ascii85_known_vector() {
    // "Hello" encodes to "87cURDZ" plus final partial group.
    let raw = b"87cURDZ~>";
    assert_eq!(decode(&dict_of("ASCII85Decode"), raw).unwrap(), b"Hello");
  }

  #[test]
  fn ascii85_zero_quad() {
    assert_eq!(decode(&dict_of("ASCII85Decode"), b"z~>").unwrap(), vec![0, 0, 0, 0]);
  }

  #[test]
  fn runlength_roundtrip() {
    let raw = [2u8, b'a', b'b', b'c', 254, b'x', 128];
    assert_eq!(decode(&dict_of("RunLengthDecode"), &raw).unwrap(), b"abcxxx".to_vec());
  }

  #[test]
  fn flate_with_chain() {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(b"chain").unwrap();
    let compressed = enc.finish().unwrap();
    let dict = PdfValue::Dict(vec![(
      "Filter".into(),
      PdfValue::Array(vec![
        PdfValue::Name("ASCIIHexDecode".into()),
        PdfValue::Name("FlateDecode".into()),
      ]),
    )]);
    let hex: String = compressed.iter().map(|b| format!("{b:02X}")).collect();
    let mut raw = hex.into_bytes();
    raw.push(b'>');
    assert_eq!(decode(&dict, &raw).unwrap(), b"chain");
  }

  #[test]
  fn lzw_short_sequence() {
    // Hand-encoded: clear, 'A', 'B', EOD at width 9.
    let mut bits: Vec<u8> = Vec::new();
    let mut acc = 0u32;
    let mut nbits = 0;
    let mut emit = |code: u32| {
      acc = (acc << 9) | code;
      nbits += 9;
      while nbits >= 8 {
        nbits -= 8;
        bits.push((acc >> nbits) as u8);
      }
    };
    emit(256);
    emit(65);
    emit(66);
    emit(257);
    if nbits > 0 {
      bits.push((acc << (8 - nbits)) as u8);
    }
    assert_eq!(lzw_decode(&bits, 1).unwrap(), b"AB");
  }

  #[test]
  fn lzw_known_answer_abab() {
    // Hand-encoded per ISO 32000 7.4.4.2 (EarlyChange 1, width 9):
    // codes 256, 65, 66, 258, 257 packed MSB-first.
    let bits = [0x80u8, 0x10, 0x48, 0x50, 0x28, 0x08];
    assert_eq!(lzw_decode(&bits, 1).unwrap(), b"ABAB");
  }

  #[test]
  fn lzw_roundtrip_early_change_0() {
    let mut x = 0x87654321u32;
    let data: Vec<u8> = (0..3000).map(|_| { x = x.wrapping_mul(1664525).wrapping_add(1013904223); (x >> 16) as u8 }).collect();
    let encoded = lzw_reference_encode_early(&data, 0);
    assert_eq!(lzw_decode(&encoded, 0).unwrap(), data);
  }

  #[test]
  fn lzw_grows_width_like_encoders() {
    // Repetitive data forces table growth past 9-bit codes.
    let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    let encoded = lzw_reference_encode(&data);
    assert_eq!(lzw_decode(&encoded, 1).unwrap(), data);
    // Random data: mostly misses, different bump alignment.
    let mut x = 0x12345678u32;
    let random: Vec<u8> = (0..3000).map(|_| { x = x.wrapping_mul(1664525).wrapping_add(1013904223); (x >> 16) as u8 }).collect();
    let encoded = lzw_reference_encode(&random);
    assert_eq!(lzw_decode(&encoded, 1).unwrap(), random);
  }

  /// Minimal spec-shaped encoder (EarlyChange selectable) used to
  /// cross-check the decoder, including 9->10 bit width growth.
  fn lzw_reference_encode(data: &[u8]) -> Vec<u8> {
    lzw_reference_encode_early(data, 1)
  }

  fn lzw_reference_encode_early(data: &[u8], early: u32) -> Vec<u8> {
    use std::collections::HashMap;
    let mut dict: HashMap<Vec<u8>, u32> = (0u32..256).map(|b| (vec![b as u8], b)).collect();
    let mut next = 258u32;
    let mut width = 9u32;
    let mut out: Vec<u8> = Vec::new();
    let mut buf = 0u32;
    let mut nbits = 0u32;
    let emit = |code: u32, width: u32, out: &mut Vec<u8>, buf: &mut u32, nbits: &mut u32| {
      *buf = ((*buf << width) | code) & 0xFFFF_FFFF;
      *nbits += width;
      while *nbits >= 8 {
        *nbits -= 8;
        out.push((*buf >> *nbits) as u8);
        *buf &= (1 << *nbits) - 1;
      }
    };
    emit(256, width, &mut out, &mut buf, &mut nbits);
    let mut w = vec![data[0]];
    for &b in &data[1..] {
      let mut wc = w.clone();
      wc.push(b);
      if dict.contains_key(&wc) {
        w = wc;
      } else {
        emit(dict[&w], width, &mut out, &mut buf, &mut nbits);
        dict.insert(wc, next);
        next += 1;
        if next == (1 << width) - early && width < 12 {
          width += 1;
        }
        w = vec![b];
      }
    }
    emit(dict[&w], width, &mut out, &mut buf, &mut nbits);
    emit(257, width, &mut out, &mut buf, &mut nbits);
    if nbits > 0 {
      out.push((buf << (8 - nbits)) as u8);
    }
    out
  }

  #[test]
  fn png_predictor_up_filter() {
    // Two rows, Up filter (tag 2): stored deltas decode to running sums.
    let raw = [2u8, 10, 20, 2, 5, 5];
    let parms = PdfValue::Dict(vec![
      ("Predictor".into(), PdfValue::Number(12.0)),
      ("Colors".into(), PdfValue::Number(1.0)),
      ("Columns".into(), PdfValue::Number(2.0)),
    ]);
    assert_eq!(apply_predictor(&raw, &parms).unwrap(), vec![10, 20, 15, 25]);
  }
}
