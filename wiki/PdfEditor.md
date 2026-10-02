# PdfEditor

`PdfEditor` rewrites content-stream bytes in place and saves the result as
an incremental update. It works from the byte ranges the interpreter
already records (see [Graphics.md](Graphics.md)), so an edit replaces the
literal that produced a run instead of rebuilding the page.

## Constructors

```rust
pub fn new(doc: PdfDocument) -> Result<Self>
```

Takes the parsed document by value: saving appends to the very bytes the
document was parsed from. Returns `Err(EncryptedEdit)` for encrypted
files (see [Errors](#errors)).

## Functions

| Function | Signature | Behavior |
|---|---|---|
| `is_dirty` | `(&self) -> bool` | True when an edit is pending |
| `edit_count` | `(&self) -> usize` | Edits since the last save |
| `document` | `(&self) -> Option<&PdfDocument>` | Document reflecting the pending edits |
| `into_document` | `(self) -> Option<PdfDocument>` | Edited document, editor consumed |
| `set_text` | `(&mut self, page: usize, run: usize, text: &str) -> Result<()>` | Replace one run's text |
| `replace_text` | `(&mut self, page: usize, find: &str, text: &str) -> Result<()>` | Replace the first run with that exact text |
| `build` | `(&mut self) -> Result<Vec<u8>>` | New file bytes without adopting them |
| `into_bytes` | `(&mut self) -> Result<Vec<u8>>` | Serialize and adopt as the new baseline |
| `save_to` | `(&mut self, path: impl AsRef<Path>) -> Result<()>` | Serialize and write |

`build` does not change the baseline, so repeated edits accumulate on top
of the original file instead of appending once per keystroke.
`into_bytes` and `save_to` do adopt.

## State model

| Stage | Meaning |
|---|---|
| `original` | The bytes as loaded, never modified |
| `pages[i].parts` | One buffer per `/Contents` entry, edited in place |
| `pages[i].dirty` | Which entries changed and need re-encoding |
| `doc` | Re-parsed from a throwaway build after every edit |

Keeping one buffer per stream is what makes spans stay valid: spans index
the concatenated page buffer, so splicing into a single shared buffer
would move every following segment and corrupt untouched streams.

## Errors

| Variant | Meaning |
|---|---|
| `EncryptedEdit` | File is encrypted; see [Limitations](#limitations) |
| `PageOutOfRange(i)` | No such page |
| `NoSuchRun(i)` | No text run with that index on the page |
| `UneditableRun(i)` | Run has no addressable source bytes |
| `RunNotFound(text)` | `replace_text` found no run with that text |
| `InvalidObject(msg)` | Span outside its stream, or crosses streams |

## Limitations

- **Encrypted files are refused.** An incremental update would have to
  re-encrypt the appended bytes with the file key; writing them as
  plaintext would silently corrupt the document.
- **No reflow.** Text positions in a content stream are absolute, so
  replacing a run moves nothing else - but a longer string simply
  overprints whatever follows it.
- **Metrics are SF Pro, not the embedded fonts.** PDFKit renders through
  the shared `FontSystem`, so an edited run is measured with substitute
  metrics. Ink positions match, glyph widths do not.
- **Images are not editable yet.** `PlacedImage::src` already records the
  `cm` that placed an image, which is what replacing or moving one needs.

## Usage / Example

```rust
use pdfkit::{PdfDocument, PdfEditor};

let doc = PdfDocument::open("/path/to/in.pdf")?;
let mut editor = PdfEditor::new(doc)?;

// Replace the first run on page 0.
editor.set_text(0, 0, "Goodbye PDF")?;
// Or find it by its current text.
editor.replace_text(0, "Hello PDF", "Goodbye PDF")?;

// The edit is visible before saving.
assert_eq!(editor.document().unwrap().page(0).unwrap().runs[0].text, "Goodbye PDF");

editor.save_to("/path/to/out.pdf")?;
# Ok::<(), pdfkit::PdfError>(())
```

## Cross References

- [IncrementalUpdate.md](IncrementalUpdate.md) – how the bytes get written
- [Graphics.md](Graphics.md) – the `OpSpan`s the edits are built on
- [DocumentStructure.md](DocumentStructure.md) – xref sections and the `/Prev` chain
- [PdfDocument.md](PdfDocument.md) – opening the document to edit