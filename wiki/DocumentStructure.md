# DocumentStructure

PDFKit parses the file structure of ISO 32000 section 7: header, body,
cross-reference data and trailer, including modern compressed files
and encrypted files.

## Header and scan fallback

Files must start with `%PDF-`. When no usable xref data exists, every
`N G obj` header is scanned so damaged files still open.

## Cross-reference tables and streams

| Feature | Behavior |
|---|---|
| Classic `xref` tables | Subsections plus `trailer /Prev` chains, newest wins |
| XRef streams | `/W`, `/Index`, `/Size`, `/Root` from the stream dict |
| `/Prev` chains | Oldest loads first so updates override correctly |
| Trailer merge | `/Root`, `/Info`, `/Encrypt`, `/ID`, `/Size` from newest |
| Object streams | `/Type /ObjStm` unpacked with an index cache |

A 20-byte table entry is `offset generation type`; the type is the third
field, and only `n` marks the object as in use. The scan fallback (used
when no xref data is usable) recovers a file by taking the *first* copy
of every object, so it ignores later definitions - that is what made
incremental updates invisible before
[IncrementalUpdate.md](IncrementalUpdate.md) existed.

## Filters

| Filter | Status |
|---|---|
| `FlateDecode` | Full, with PNG optimum and TIFF predictors |
| `LZWDecode` | Full, `EarlyChange` 0 and 1 with width growth |
| `ASCIIHexDecode` | Full |
| `ASCII85Decode` | Full, `z` and `~>` handled |
| `RunLengthDecode` | Full |
| `DCTDecode`, `JPXDecode` | Passthrough (image data stays compressed) |
| `CCITTFaxDecode`, `JBIG2Decode` | `Err(UnsupportedFilter)` |

Multi-filter chains and `/DecodeParms` arrays decode in order.

`FlateDecode` is handled by `archivekit::zlib_decompress_unverified_limited`
with a size cap. The Adler-32 trailer is **not** checked: many PDF producers
ship a zeroed or stale one, and the previous `flate2`-based decoder ignored it
too, so this keeps the same files readable.

## Encryption

`FileParser::new` opens with the empty password;
`FileParser::new_with_password` takes an explicit one. Empty-password
rejections yield `NeedsPassword`, explicit ones `WrongPassword`.

| Handler | Status |
|---|---|
| V1/V2/V3 RC4 | Full, key lengths 40-128 bit |
| V4 AESV2/Identity | Full via `/CF`, `/StmF`, `/StrF` |
| V5 AES-256 R5/R6 | Full, iterative SHA hash, user and owner paths |
| Other filters/handlers | `Err(UnsupportedCrypt)` |

Streams decrypt with the stream filter, strings with the string
filter; the `/Encrypt` dict, trailer `/ID` and xref streams stay raw.
Objects inside object streams use their stream's number.

## Content segments

A page's `/Contents` may be one stream or an array of streams. PDFKit
decodes every entry and concatenates them with a newline separator
(ISO 32000 7.8.2: the array is logically a single stream), then records
where each stream landed.

```rust
pub struct ContentSegment {
  /// Object number of the content stream.
  pub obj: u32,
  /// First byte of this stream inside the concatenated buffer.
  pub start: usize,
  /// One past the last byte of this stream.
  pub end: usize,
}
```

| Function | Behavior |
|---|---|
| `ContentSegment::container_of` | Object number covering a byte offset, `0` outside every segment |

The trailing separator after the last stream lies outside every
segment. `ParsedPage` and `PdfPage` both expose `content` plus
`content_segments`, so an editor can map an item back to the exact
stream object it has to re-encode.

## Types

```rust
pub struct FileParser { /* offsets, trailer, crypt */ }
```

```rust
pub fn new(data: Vec<u8>) -> Result<Self>
```

```rust
pub fn new_with_password(data: Vec<u8>, password: &[u8]) -> Result<Self>
```

```rust
pub struct CryptState { /* file_key, filters */ }
```

## Usage / Example

```rust
use pdfkit::FileParser;

let parser = FileParser::new_with_password(bytes, b"userpass").unwrap();
let pages = parser.pages().unwrap();
```

Real-file fixtures live in `tests/fixtures` (CC0 pdfbin.net files
covering RC4-40, RC4-128, AES-128 and AES-256 with user, owner and
empty passwords).

## Cross References

- [PdfDocument.md](PdfDocument.md) – loading API and error variants
- [Images.md](Images.md) – how image streams reuse these filters
- [IncrementalUpdate.md](IncrementalUpdate.md) – writing new xref sections
