use aes::Aes128;
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyInit, generic_array::GenericArray};
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::error::{PdfError, Result};
use crate::objects::PdfValue;

/// Password padding string (ISO 32000 7.6.3.3.2, step a).
pub const PADDING: [u8; 32] = [
  0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, //
  0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// RC4 stream cipher (symmetric; used for V1/V2 object encryption).
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
  let mut s: [u8; 256] = core::array::from_fn(|i| i as u8);
  let mut j = 0u8;
  for i in 0..256 {
    j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len().max(1)]);
    s.swap(i, j as usize);
  }
  let (mut i, mut j) = (0u8, 0u8);
  data
    .iter()
    .map(|b| {
      i = i.wrapping_add(1);
      j = j.wrapping_add(s[i as usize]);
      s.swap(i as usize, j as usize);
      b ^ s[(s[i as usize].wrapping_add(s[j as usize])) as usize]
    })
    .collect()
}

/// MD5 digest.
pub fn md5sum(data: &[u8]) -> [u8; 16] {
  md5::compute(data).into()
}

/// SHA-256 digest.
pub fn sha256sum(data: &[u8]) -> [u8; 32] {
  Sha256::digest(data).into()
}

/// AES-128-CBC encrypt without padding (data must fill blocks).
pub fn aes128_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
  if data.len() % 16 != 0 || key.len() < 16 || iv.len() < 16 {
    return None;
  }
  let cipher = Aes128::new(GenericArray::from_slice(&key[..16]));
  let mut cipher = cipher;
  let mut prev = iv[..16].to_vec();
  let mut out = Vec::with_capacity(data.len());
  for block in data.chunks(16) {
    let xored: Vec<u8> = block.iter().zip(prev.iter()).map(|(a, b)| a ^ b).collect();
    let mut buf = GenericArray::clone_from_slice(&xored);
    cipher.encrypt_block_mut(&mut buf);
    out.extend_from_slice(&buf);
    prev = buf.to_vec();
  }
  Some(out)
}

/// AES-128-CBC decrypt without unpadding.
pub fn aes128_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
  if data.len() % 16 != 0 || key.len() < 16 || iv.len() < 16 {
    return None;
  }
  let cipher = Aes128::new(GenericArray::from_slice(&key[..16]));
  let mut cipher = cipher;
  let mut prev = iv[..16].to_vec();
  let mut out = Vec::with_capacity(data.len());
  for block in data.chunks(16) {
    let mut buf = GenericArray::clone_from_slice(block);
    cipher.decrypt_block_mut(&mut buf);
    out.extend(buf.iter().zip(prev.iter()).map(|(a, b)| a ^ b));
    prev = block.to_vec();
  }
  Some(out)
}

/// AES-256-CBC decrypt without unpadding (V5 file key recovery).
pub fn aes256_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
  use aes::Aes256;
  if data.len() % 16 != 0 || key.len() < 32 || iv.len() < 16 {
    return None;
  }
  let cipher = Aes256::new(GenericArray::from_slice(&key[..32]));
  let mut cipher = cipher;
  let mut prev = iv[..16].to_vec();
  let mut out = Vec::with_capacity(data.len());
  for block in data.chunks(16) {
    let mut buf = GenericArray::clone_from_slice(block);
    cipher.decrypt_block_mut(&mut buf);
    out.extend(buf.iter().zip(prev.iter()).map(|(a, b)| a ^ b));
    prev = block.to_vec();
  }
  Some(out)
}

/// Stream/string cipher for one crypt filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cfm {
  /// No encryption (identity).
  None,
  /// RC4 with the object key (V1/V2).
  Rc4,
  /// AES-128-CBC with IV prefix (AESV2).
  Aes128,
}

/// Active decryption state for a file.
#[derive(Debug, Clone)]
pub struct CryptState {
  /// True for V5 (AES-256, direct file key, no object derivation).
  pub v5: bool,
  /// File encryption key (V1-4: `n` bytes; V5: 32 bytes).
  pub file_key: Vec<u8>,
  /// Key length in bytes for object derivation (V1-4).
  pub key_len: usize,
  /// Stream cipher selector.
  pub stm: Cfm,
  /// String cipher selector.
  pub strm: Cfm,
  /// Object number of the `/Encrypt` dict (never decrypted itself).
  pub encrypt_num: u32,
}

impl CryptState {
  /// Open an `/Encrypt` dict (already resolved) with a password.
  /// Returns `WrongPassword` when neither user nor owner auth passes.
  pub fn open(encrypt: &PdfValue, id0: &[u8], password: &[u8], encrypt_num: u32) -> Result<Self> {
    let v = encrypt.get("V").and_then(|v| v.as_number()).unwrap_or(0.0) as i64;
    if v == 5 {
      return Self::open_v5(encrypt, password, encrypt_num);
    }
    if !(1..=4).contains(&v) {
      return Err(PdfError::UnsupportedCrypt(format!("V{v}")));
    }
    let r = encrypt.get("R").and_then(|v| v.as_number()).unwrap_or(2.0) as i64;
    if !(2..=4).contains(&r) {
      return Err(PdfError::UnsupportedCrypt(format!("R{r}")));
    }
    let length = encrypt.get("Length").and_then(|v| v.as_number()).unwrap_or(40.0) as usize / 8;
    let n = length.clamp(5, 16);
    let o = bytes_of(encrypt.get("O"))?;
    let u = bytes_of(encrypt.get("U"))?;
    let p = encrypt.get("P").and_then(|v| v.as_number()).unwrap_or(0.0) as i32;
    let metadata = encrypt.get("EncryptMetadata").and_then(|v| match v {
      PdfValue::Bool(b) => Some(*b),
      _ => None,
    }).unwrap_or(true);
    let (stm, strm) = crypt_filters(encrypt, v);
    // User password first.
    if let Some(key) = file_key(password, &o, p as u32, id0, r, n, metadata) {
      if verify_user(&key, &u, id0, r) {
        return Ok(Self { v5: false, file_key: key, key_len: n, stm, strm, encrypt_num });
      }
    }
    // Owner password derives the user password (Algorithm 7).
    if let Some(user_pad) = owner_to_user(password, &o, r, n) {
      if let Some(key) = file_key(&user_pad, &o, p as u32, id0, r, n, metadata) {
        if verify_user(&key, &u, id0, r) {
          return Ok(Self { v5: false, file_key: key, key_len: n, stm, strm, encrypt_num });
        }
      }
    }
    Err(PdfError::WrongPassword)
  }

  fn open_v5(encrypt: &PdfValue, password: &[u8], encrypt_num: u32) -> Result<Self> {
    let r = encrypt.get("R").and_then(|v| v.as_number()).unwrap_or(6.0) as i64;
    if r != 6 && r != 5 {
      return Err(PdfError::UnsupportedCrypt(format!("R{r}")));
    }
    let pw: Vec<u8> = password.iter().copied().take(127).collect();
    let o = bytes_of(encrypt.get("O"))?;
    let u = bytes_of(encrypt.get("U"))?;
    if o.len() < 48 || u.len() < 48 {
      return Err(PdfError::InvalidObject("short O/U for V5".into()));
    }
    let r6 = r == 6;
    // User path (Algorithm 8).
    let hash = pdf20_hash(&pw, &[pw.as_slice(), &u[32..40]].concat(), &[], r6);
    if hash == u[..32] {
      let ue_key = pdf20_hash(&pw, &[pw.as_slice(), &u[40..48]].concat(), &[], r6);
      let ue = bytes_of(encrypt.get("UE"))?;
      let file_key = aes256_cbc_decrypt(&ue_key, &[0u8; 16], &ue).ok_or(PdfError::WrongPassword)?;
      return Ok(Self { v5: true, file_key, key_len: 32, stm: Cfm::Aes128, strm: Cfm::Aes128, encrypt_num });
    }
    // Owner path (Algorithm 9).
    let hash = pdf20_hash(&pw, &[pw.as_slice(), &o[32..40], &u[..48]].concat(), &u[..48], r6);
    if hash == o[..32] {
      let oe_key = pdf20_hash(&pw, &[pw.as_slice(), &o[40..48], &u[..48]].concat(), &u[..48], r6);
      let oe = bytes_of(encrypt.get("OE"))?;
      let file_key = aes256_cbc_decrypt(&oe_key, &[0u8; 16], &oe).ok_or(PdfError::WrongPassword)?;
      return Ok(Self { v5: true, file_key, key_len: 32, stm: Cfm::Aes128, strm: Cfm::Aes128, encrypt_num });
    }
    Err(PdfError::WrongPassword)
  }

  /// Derive the per-object key (Algorithm 2.1, V1-4). V5 ignores it.
  pub fn object_key(&self, num: u32, gen: u16, aes: bool) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&self.file_key);
    data.extend_from_slice(&num.to_le_bytes()[..3]);
    data.extend_from_slice(&gen.to_le_bytes()[..2]);
    if aes {
      data.extend_from_slice(&[0x73, 0x41, 0x6C, 0x54]);
    }
    let hash = md5sum(&data);
    hash[..(self.key_len + 5).min(16)].to_vec()
  }

  /// Decrypt stream bytes with the stream filter.
  pub fn decrypt_stream(&self, data: &[u8], num: u32, gen: u16) -> Option<Vec<u8>> {
    match self.stm {
      Cfm::None => Some(data.to_vec()),
      Cfm::Rc4 => Some(rc4(&self.object_key(num, gen, false), data)),
      Cfm::Aes128 => {
        if self.v5 {
          let (iv, rest) = data.split_at_checked(16)?;
          aes256_cbc_decrypt(&self.file_key, iv, rest)
        } else {
          let (iv, rest) = data.split_at_checked(16)?;
          aes128_cbc_decrypt(&self.object_key(num, gen, true), iv, rest)
        }
      }
    }
  }

  /// Decrypt a string with the string filter.
  pub fn decrypt_string(&self, data: &[u8], num: u32, gen: u16) -> Option<Vec<u8>> {
    match self.strm {
      Cfm::None => Some(data.to_vec()),
      Cfm::Rc4 => Some(rc4(&self.object_key(num, gen, false), data)),
      Cfm::Aes128 => {
        if self.v5 {
          let (iv, rest) = data.split_at_checked(16)?;
          aes256_cbc_decrypt(&self.file_key, iv, rest)
        } else {
          let (iv, rest) = data.split_at_checked(16)?;
          aes128_cbc_decrypt(&self.object_key(num, gen, true), iv, rest)
        }
      }
    }
  }

  /// Decrypt all strings in a parsed object value (except the
  /// `/Encrypt` dict itself, selected by `encrypt_num`).
  pub fn decrypt_value(&self, value: PdfValue, num: u32, gen: u16) -> PdfValue {
    if num == self.encrypt_num {
      return value;
    }
    self.decrypt_value_inner(value, num, gen)
  }

  fn decrypt_value_inner(&self, value: PdfValue, num: u32, gen: u16) -> PdfValue {
    match value {
      PdfValue::Str(bytes) => PdfValue::Str(self.decrypt_string(&bytes, num, gen).unwrap_or(bytes)),
      PdfValue::Hex(bytes) => PdfValue::Hex(self.decrypt_string(&bytes, num, gen).unwrap_or(bytes)),
      PdfValue::Array(items) => {
        PdfValue::Array(items.into_iter().map(|v| self.decrypt_value_inner(v, num, gen)).collect())
      }
      PdfValue::Dict(entries) => PdfValue::Dict(
        entries.into_iter().map(|(k, v)| (k, self.decrypt_value_inner(v, num, gen))).collect(),
      ),
      other => other,
    }
  }
}

fn bytes_of(value: Option<&PdfValue>) -> Result<Vec<u8>> {
  match value {
    Some(PdfValue::Str(bytes)) | Some(PdfValue::Hex(bytes)) => Ok(bytes.clone()),
    _ => Err(PdfError::InvalidObject("crypt entry must be a string".into())),
  }
}

fn crypt_filters(encrypt: &PdfValue, v: i64) -> (Cfm, Cfm) {
  let cf = encrypt.get("CF").and_then(|v| match v {
    PdfValue::Dict(_) => Some(v),
    _ => None,
  });
  let cfm_of = |name: &str| match cf.and_then(|cf| cf.get(name)) {
    Some(PdfValue::Name(n)) => match n.as_str() {
      "Identity" => Cfm::None,
      "V2" => Cfm::Rc4,
      "AESV2" | "AESV3" => Cfm::Aes128,
      _ => Cfm::Rc4,
    },
    _ => {
      if v <= 3 {
        Cfm::Rc4
      } else {
        Cfm::None
      }
    },
  };
  // StmF/StrF name entries select from /CF; missing means the default.
  let pick = |key: &str| match encrypt.get(key) {
    Some(PdfValue::Name(n)) if cf.is_some() => cfm_of(n),
    _ => {
      if v <= 3 {
        Cfm::Rc4
      } else {
        Cfm::None
      }
    }
  };
  // Resolve the named filter's CFM through /CF.
  let resolve = |key: &str| match encrypt.get(key).and_then(|v| v.as_name()) {
    Some(filter) => match cf.and_then(|cf| cf.get(filter)) {
      Some(PdfValue::Dict(_)) => {
        let dict = cf.and_then(|cf| cf.get(filter)).cloned().unwrap_or(PdfValue::Null);
        match dict.get("CFM").and_then(|v| v.as_name()) {
          Some("Identity") => Cfm::None,
          Some("V2") => Cfm::Rc4,
          Some("AESV2") | Some("AESV3") => Cfm::Aes128,
          _ => Cfm::Rc4,
        }
      }
      _ => pick(key),
    },
    None => pick(key),
  };
  let _ = cfm_of;
  (resolve("StmF"), resolve("StrF"))
}

fn pad_password(password: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(32);
  out.extend_from_slice(password);
  out.extend_from_slice(&PADDING);
  out.truncate(32);
  out
}

/// File encryption key (Algorithm 2.4).
fn file_key(password: &[u8], o: &[u8], p: u32, id0: &[u8], r: i64, n: usize, metadata: bool) -> Option<Vec<u8>> {
  let mut data = pad_password(password);
  data.extend_from_slice(o);
  data.extend_from_slice(&p.to_le_bytes());
  data.extend_from_slice(id0);
  if r >= 4 && !metadata {
    data.extend_from_slice(&[0xFF; 4]);
  }
  let mut hash = md5sum(&data);
  if r >= 3 {
    for _ in 0..50 {
      hash = md5sum(&hash[..n]);
    }
  }
  Some(hash[..n].to_vec())
}

/// User password check (Algorithms 2.5/2.6).
fn verify_user(key: &[u8], u: &[u8], id0: &[u8], r: i64) -> bool {
  if r == 2 {
    return u.len() >= 32 && rc4(key, &PADDING) == u[..32];
  }
  let mut data = PADDING.to_vec();
  data.extend_from_slice(id0);
  let mut out = rc4(key, &md5sum(&data));
  for i in 1..20u8 {
    let xorkey: Vec<u8> = key.iter().map(|b| b ^ i).collect();
    out = rc4(&xorkey, &out);
  }
  u.len() >= 16 && out == u[..16]
}

/// Owner password to padded user password (Algorithm 2.7).
fn owner_to_user(password: &[u8], o: &[u8], r: i64, n: usize) -> Option<Vec<u8>> {
  let mut key = md5sum(&pad_password(password));
  if r >= 3 {
    for _ in 0..50 {
      key = md5sum(&key[..n]);
    }
  }
  let key = key[..n].to_vec();
  let mut out = o.to_vec();
  if r == 2 {
    out = rc4(&key, &out);
  } else {
    for i in (0..20u8).rev() {
      let xorkey: Vec<u8> = key.iter().map(|b| b ^ i).collect();
      out = rc4(&xorkey, &out);
    }
  }
  Some(out)
}

/// PDF 2.0 iterative hash (Algorithm 2.B). `r6` selects the 64-round
/// loop; R5 uses a single SHA-256 pass.
pub fn pdf20_hash(password: &[u8], data: &[u8], user_bytes: &[u8], r6: bool) -> Vec<u8> {
  let mut k: Vec<u8> = sha256sum(data).to_vec();
  if !r6 {
    return k;
  }
  let mut round = 0u32;
  let mut e = vec![0u8];
  loop {
    let mut k1 = Vec::with_capacity((password.len() + k.len() + user_bytes.len()) * 64);
    for _ in 0..64 {
      k1.extend_from_slice(password);
      k1.extend_from_slice(&k);
      k1.extend_from_slice(user_bytes);
    }
    let encrypted = aes128_cbc_encrypt(&k[..16.min(k.len())], &k[16.min(k.len())..32.min(k.len())], &k1);
    e = encrypted.unwrap_or_default();
    let sum: u32 = e.iter().take(16).map(|b| u32::from(*b)).sum();
    k = match sum % 3 {
      0 => Sha256::digest(&e).to_vec(),
      1 => Sha384::digest(&e).to_vec(),
      _ => Sha512::digest(&e).to_vec(),
    };
    round += 1;
    if round >= 64 && e.last().copied().unwrap_or(0) <= (round - 32) as u8 {
      break;
    }
  }
  k[..32.min(k.len())].to_vec()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rc4_known_vector() {
    assert_eq!(hex(&rc4(b"Key", b"Plaintext")), "bbf316e8d940af0ad3");
  }

  #[test]
  fn md5_known_vector() {
    assert_eq!(hex(&md5sum(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
  }

  #[test]
  fn sha256_known_vector() {
    assert_eq!(hex(&sha256sum(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  }

  #[test]
  fn aes_cbc_roundtrip() {
    let key = b"0123456789abcdef";
    let iv = b"abcdef0123456789";
    let data = b"0123456789abcdef0123456789abcdef";
    let enc = aes128_cbc_encrypt(key, iv, data).unwrap();
    assert_eq!(aes128_cbc_decrypt(key, iv, &enc).unwrap(), data);
  }

  #[test]
  fn pdf20_hash_deterministic() {
    let a = pdf20_hash(b"userpass", b"userpassSALTSALT", &[], true);
    let b = pdf20_hash(b"userpass", b"userpassSALTSALT", &[], true);
    assert_eq!(a, b);
    assert_eq!(a.len(), 32);
  }

  fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
  }

  fn fixture(name: &str) -> Vec<u8> {
    // Unit tests run with the crate root as working directory.
    let path = format!("tests/fixtures/{name}");
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing fixture {path}"))
  }

  #[test]
  fn encrypted_without_password_needs_password() {
    let data = fixture("aes128-user.pdf");
    assert_eq!(
      crate::document::PdfDocument::load_bytes(data).unwrap_err(),
      PdfError::NeedsPassword
    );
  }

  #[test]
  fn encrypted_wrong_password_fails() {
    let data = fixture("aes128-user.pdf");
    assert_eq!(
      crate::document::PdfDocument::load_bytes_with_password(data, "nope").unwrap_err(),
      PdfError::WrongPassword
    );
  }

  #[test]
  fn aes128_user_password_opens() {
    let data = fixture("aes128-user.pdf");
    let doc = crate::document::PdfDocument::load_bytes_with_password(data, "userpass").unwrap();
    assert_eq!(doc.page_count(), 1);
    let page = doc.page(0).unwrap();
    assert!((page.width - 595.28).abs() < 1.0);
    assert!((page.height - 841.89).abs() < 1.0);
  }

  #[test]
  fn aes128_owner_password_opens() {
    let data = fixture("aes128-user.pdf");
    let doc = crate::document::PdfDocument::load_bytes_with_password(data, "ownerpass").unwrap();
    assert_eq!(doc.page_count(), 1);
  }

  #[test]
  fn aes128_owner_only_opens_empty() {
    let data = fixture("aes128-owner.pdf");
    let doc = crate::document::PdfDocument::load_bytes(data).unwrap();
    assert_eq!(doc.page_count(), 1);
  }

  #[test]
  fn aes256_user_and_owner_open() {
    for password in ["userpass", "ownerpass"] {
      let data = fixture("aes256-user.pdf");
      let doc = crate::document::PdfDocument::load_bytes_with_password(data, password).unwrap();
      assert_eq!(doc.page_count(), 1, "password={password}");
    }
  }

  #[test]
  fn aes256_both_passwords_open() {
    for password in ["userpass", "ownerpass"] {
      let data = fixture("aes256-both.pdf");
      let doc = crate::document::PdfDocument::load_bytes_with_password(data, password).unwrap();
      assert_eq!(doc.page_count(), 1, "password={password}");
    }
  }

  #[test]
  fn aes256_owner_only_opens_empty() {
    let data = fixture("aes256-owner.pdf");
    let doc = crate::document::PdfDocument::load_bytes(data).unwrap();
    assert_eq!(doc.page_count(), 1);
  }

  #[test]
  fn rc4_owner_only_open_empty() {
    for name in ["rc4-40-owner.pdf", "rc4-128-owner.pdf"] {
      let data = fixture(name);
      let doc = crate::document::PdfDocument::load_bytes(data).unwrap();
      assert_eq!(doc.page_count(), 1, "{name}");
    }
  }
}
