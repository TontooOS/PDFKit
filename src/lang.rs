use std::collections::HashMap;
use std::sync::OnceLock;

use crate::error::{PdfError, Result};

static EN_US: &str = include_str!("../lang/en_us.json");
static DE_DE: &str = include_str!("../lang/de_de.json");

fn table(locale: &str) -> &'static HashMap<String, String> {
  static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
  static DE: OnceLock<HashMap<String, String>> = OnceLock::new();
  if locale == "de_de" {
    DE.get_or_init(|| parse_table(DE_DE))
  } else {
    EN.get_or_init(|| parse_table(EN_US))
  }
}

fn parse_table(raw: &str) -> HashMap<String, String> {
  serde_json::from_str(raw).unwrap_or_default()
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
    serde_json::from_str::<HashMap<String, String>>(raw)
      .map_err(|e| PdfError::Lang(format!("{name}: {e}")))?;
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
}
