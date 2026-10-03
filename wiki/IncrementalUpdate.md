# IncrementalUpdate

The `writer` module appends edited objects to an existing PDF instead of
rewriting it (ISO 32000 section 7.5.6). The original bytes stay
byte-for-byte intact at the front of the file, so anything the parser
does not model survives untouched - a full rewrite would have to
serialize every object again and would lose whatever it cannot
represent.

## Constructors

```rust
pub fn new(original: Vec<u8>, prev_startxref: u64, prev_is_stream: bool, trailer: Trailer, next_num: u32) -> Self
```

| Parameter | Meaning |
|---|---|
| `original` | Bytes to append to |
| `prev_startxref` | Offset of the last section, written as `/Prev` |
| `prev_is_stream` | Whether that section is a stream; mirrored by the new one |
| `trailer` | `/Root`, `/Info`, `/Encrypt`, `/ID`, `/Size` to carry forward |
| `next_num` | First free object number |

## Functions

| Function | Signature | Behavior |
|---|---|---|
| `next_num` | `(&self) -> u32` | Next free object number |
| `push_raw` | `(&mut self, num: u32, gen: u16, body: Vec<u8>)` | Append an object body verbatim |
| `push_stream` | `(&mut self, dict_entries: &[(&str, String)], data: &[u8]) -> u32` | Append a Flate stream at a fresh number |
| `encode_value` | `(value: &PdfValue) -> Vec<u8>` | Serialize a value as an object body |
| `finish` | `(self) -> Result<Vec<u8>>` | Append objects plus a new xref section |

`finish` returns the original bytes unchanged when nothing was pushed.

## What gets written

1. A newline, if the previous file did not end with one.
2. Every appended object in ascending number order.
3. A cross-reference section, then `trailer` and `startxref`.

The section mirrors the original's flavor: a file that used an xref table
gets a table, a file that used an xref stream gets a `/Type /XRef` stream
with `/W [1 4 2]`. Mixing the two is what hybrid-reference files do and
readers disagree about it.

## Rules that matter

| Rule | Why |
|---|---|
| One subsection per run of consecutive object numbers | A section describes objects, not a range with holes. Marking a gap free would make readers drop an untouched object, because the newest section wins |
| `/Size` is one past the highest object number written | Readers reject a file whose `/Size` does not cover its own objects |
| An xref stream lists itself | It gets a fresh object number and its own entry, at an offset known before it is written |
| `/Prev` always chains | Without it the appended section replaces the whole file |
| A space separates every dictionary key from its value | `/Parent` followed by `2 0 R` merges into the single name `Parent2` |

## Types

```rust
pub struct Trailer {
  pub root: Option<(u32, u16)>,
  pub info: Option<(u32, u16)>,
  pub encrypt: Option<(u32, u16)>,
  pub id: Option<Vec<u8>>,
  pub size: u32,
}
```

`filter::deflate` does the compression; it is the only encoder in the
crate, used solely by this writer. It calls
`archivekit::zlib_compress(data, CompressionLevel::Balanced)`.

## Usage / Example

Appending an object to a file by hand:

```rust
use pdfkit::{PdfDocument, PdfError, UpdateWriter};

let doc = PdfDocument::load_file("/path/to/in.pdf")?;
let mut writer = UpdateWriter::new(
  doc.bytes().to_vec(),
  doc.startxref(),
  doc.xref_is_stream(),
  doc.trailer().clone(),
  doc.next_object_number(),
);
let num = writer.push_stream(&[], b"BT /F1 12 Tf 72 720 Td (new) Tj ET");
println!("content stream written as object {num}");
let _ = writer.finish()?;
# Ok::<(), PdfError>(())
```

See [PdfEditor.md](PdfEditor.md) for the real caller, and the
`editdemo` example for an end-to-end run.

## Cross References

- [PdfEditor.md](PdfEditor.md) – the mutations this writer serializes
- [DocumentStructure.md](DocumentStructure.md) – reading xref sections back
- [Graphics.md](Graphics.md) – the spans that decide what changes