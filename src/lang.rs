use std::collections::HashMap;
use std::sync::OnceLock;

use crate::error::{PdfError, Result};
use foundation::serialization::JsonValue;

static EN_US: &str = include_str!("../lang/en_us.json");
static DE_DE: &str = include_str!("../lang/de_de.json");

fn table(locale: &str) -> &'static HashMap<String, String> {
  static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
  static DE: OnceLock<HashMap<String, String>> = OnceLock::new();
  if locale == "de_de" {
    DE.get_or_init(|| parse_table(DE_DE).unwrap_or_default())
  } else {
    EN.get_or_init(|| parse_table(EN_US).unwrap_or_default())
  }
}

/// Parse a flat `{"key": "value"}` language table. Nested values, arrays and
/// non-string scalars are skipped so a malformed entry cannot break the UI.
fn parse_table(raw: &str) -> Result<HashMap<String, String>> {
  let doc = JsonValue::parse(raw).map_err(|e| PdfError::Lang(e.to_string()))?;
  let entries = doc
    .object_entries()
    .ok_or_else(|| PdfError::Lang("language file must be a JSON object".into()))?;
  Ok(
    entries
      .iter()
      .filter_map(|(key, value)| value.as_str().map(|s| (key.clone(), s.to_string())))
      .collect(),
  )
}

/// Look up a localized string.
///
/// `locale` is `"en_us"` or `"de_de"` (system locale, anything else
/// falls back to English). Falls back to the English table and finally
/// to the key itself so the UI never shows an empty label.
pub fn get(key: &str, locale: &str) -> String {
  if let Some(value) = table(locale).get(key) {
    return value.clone();
  }
  if locale != "en_us" {
    if let Some(value) = table("en_us").get(key) {
      return value.clone();
    }
  }
  key.to_owned()
}

/// Detect the system locale from `LANG`/`LC_ALL` (e.g. `de_DE.UTF-8`
/// maps to `de_de`). Defaults to `en_us`.
pub fn system_locale() -> String {
  for var in ["LC_ALL", "LANG"] {
    if let Ok(raw) = std::env::var(var) {
      let lower = raw.to_lowercase();
      if lower.starts_with("de") {
        return "de_de".into();
      }
      break;
    }
  }
  "en_us".into()
}

/// Validate both language files (used by tests and the viewer).
pub fn validate() -> Result<()> {
  for (name, raw) in [("en_us", EN_US), ("de_de", DE_DE)] {
    parse_table(raw).map_err(|e| PdfError::Lang(format!("{name}: {e}")))?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn falls_back_to_key() {
    assert_eq!(get("missing.key", "en_us"), "missing.key");
  }

  #[test]
  fn both_files_parse() {
    validate().unwrap();
    assert!(!get("pdfkit.app.title", "en_us").is_empty());
    assert!(!get("pdfkit.app.title", "de_de").is_empty());
  }

  #[test]
  fn flat_tables_are_read() {
    let table = parse_table(r#"{"a":"one","b":"two"}"#).unwrap();
    assert_eq!(table.get("a").map(String::as_str), Some("one"));
    assert_eq!(table.len(), 2);
  }

  #[test]
  fn non_string_values_are_skipped() {
    let table = parse_table(r#"{"a":"one","n":1,"nested":{"x":"y"},"arr":[1]}"#).unwrap();
    assert_eq!(table.len(), 1);
    assert_eq!(table.get("a").map(String::as_str), Some("one"));
  }

  #[test]
  fn malformed_files_are_rejected() {
    assert!(parse_table("not json").is_err());
    assert!(parse_table("[]").is_err());
  }
}
