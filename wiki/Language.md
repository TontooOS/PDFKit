# Language

PDFKit ships `lang/en_us.json` and `lang/de_de.json` with the user-facing
viewer and error strings. Only these two locales exist; the system locale
selects between them.

## Functions

```rust
pub fn get(key: &str, locale: &str) -> String
```

Returns the localized string for `key`. Falls back to the English table
and finally to the key itself, so the UI never shows an empty label.
Returns the key unchanged when it is missing everywhere.

```rust
pub fn system_locale() -> String
```

Detects the system locale from `LC_ALL`/`LANG` (`de*` maps to `de_de`,
everything else to `en_us`).

```rust
pub fn validate() -> Result<()>
```

Parses both language files. Returns `Err(Lang)` with a detail message
when a file is not valid JSON.

## Keys

| Key | English | German |
|---|---|---|
| `pdfkit.app.title` | `PDFKit` | `PDFKit` |
| `pdfkit.app.description` | `Native PDF viewer for TontooOS` | `Nativer PDF-Betrachter fuer TontooOS` |
| `pdfkit.view.empty` | `No document loaded` | `Kein Dokument geladen` |
| `pdfkit.view.page` | `Page {} of {}` | `Seite {} von {}` |
| `pdfkit.view.zoom_in` | `Zoom in` | `Vergroessern` |
| `pdfkit.view.zoom_out` | `Zoom out` | `Verkleinern` |
| `pdfkit.error.load_failed` | `Could not open PDF: '{}'` | `PDF konnte nicht geoeffnet werden: '{}'` |
| `pdfkit.error.no_text` | `This page contains no text yet` | `Diese Seite enthaelt noch keinen Text` |
| `pdfkit.error.needs_password` | `This PDF is encrypted, a password is required` | `Dieses PDF ist verschluesselt, ein Passwort wird benoetigt` |
| `pdfkit.error.wrong_password` | `Wrong password` | `Falsches Passwort` |

## Usage / Example

```rust
use pdfkit::lang;

let locale = lang::system_locale();
let label = lang::get("pdfkit.view.empty", &locale);
```

## Cross References

- [PdfView.md](PdfView.md) – view that shows these strings
- [PdfDocument.md](PdfDocument.md) – error variants behind the keys
