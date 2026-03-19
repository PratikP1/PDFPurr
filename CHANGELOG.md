# Changelog

All notable changes to PDFPurr will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-03-19

Initial release.

### Reading and Parsing
- Full PDF 1.0-2.0 parser with xref table and xref stream support
- Automatic xref repair by scanning for object definitions when xref is corrupt
- Stream recovery: falls back to endstream scanning when /Length is wrong
- Circular reference protection: depth limit 64 on page tree traversal
- Incremental update support (follows /Prev chain with depth limit 32)
- Lazy object loading via `from_bytes_lazy()` for large files
- Memory-mapped file access via `open_mmap()`

### Text Extraction
- Font-aware extraction using WinAnsi, MacRoman, PDFDocEncoding, Standard, and MacExpert encodings
- ToUnicode CMap parsing for character code to Unicode mapping
- CJK text support through CIDFont handling

### Image Extraction
- Raw, JPEG (DCTDecode), JPEG2000 (JPXDecode), CCITT fax, LZW, FlateDecode, ASCIIHex, ASCII85, RunLength
- Checked arithmetic on all image dimensions to prevent overflow attacks

### Encryption
- Standard security handler revisions R2 through R6
- RC4 (40/128-bit), AES-128, AES-256
- R5 SHA-256 key derivation and R6 Algorithm 2.B iterative hash

### Rendering
- Page-to-pixel rendering via tiny-skia
- Full ISO 32000-2 content stream operator support
- Paths, text, images, shading (axial, radial, function-based), tiling patterns
- Transparency groups, soft masks, blend modes, clipping
- Font rendering: Standard 14, embedded TrueType/OpenType (skrifa), CIDFont, Type 3, Type 1
- Color spaces: DeviceRGB/Gray/CMYK, CalGray/CalRGB, Lab, Indexed, Separation, DeviceN, ICCBased
- Annotation rendering with appearance stream (/AP/N) support, fallback to Link (blue) and Highlight (yellow) overlays

### Writing
- `to_bytes()` for standard PDF serialization
- `to_linearized_bytes()` for Fast Web View optimization
- `to_incremental_update()` to preserve original bytes and digital signatures
- `save()` to disk

### Page Manipulation
- Add, remove, reorder, rotate, merge pages

### Font Embedding
- TrueType/OpenType subsetting via allsorts
- CIDFont for CJK and large glyph sets (up to 65535 glyphs)
- Variable font axis support via `from_ttf_with_axes()`
- ToUnicode CMap generation for text extraction from generated PDFs

### Forms and Signatures
- AcroForms: read field names/types/values, set field values
- Digital signatures: parse and verify integrity

### Standards Validation
- PDF/A: levels 1a, 1b, 2a, 2b, 2u, 3a, 3b, 3u
- PDF/X: 1a, 3, 4
- PDF/UA: tagged PDF validation, structure tree inspection

### Annotations and Outlines
- Annotations: subtype, rect, contents, URI, flags (Hidden/Print/ReadOnly), color, author, modification date, QuadPoints
- Outlines: title, destination page, URI, action type, color, bold/italic flags

### Metadata
- Info dictionary extraction (all 8 standard fields)
- XMP metadata with namespace-aware XML parsing (rdf:Alt, rdf:Seq, rdf:Description attributes)
- Merge strategy for Info + XMP sources

### Security
- Zero `panic!` or `unreachable!()` in production code
- Checked arithmetic on image dimensions and buffer allocations
- Resource limits: 256MB decoded stream max, 1M xref rebuild cap, 16MB predictor row cap
- Depth limits: page tree (64), outlines (32), inherited properties (32)
- Iteration limits: outline siblings (10,000)
- Predictor parameter clamping (Columns, Colors, BitsPerComponent)

### Testing
- 912+ tests across unit, adversarial, corpus, integration, property-based, and doc-test suites
- 3 cargo-fuzz targets (document, content stream, object parser)
- 18 proptest fuzz targets with targeted mutation strategies
- 22 adversarial edge-case tests (truncation, deep nesting, self-references, huge IDs, corrupt files)
- 33+ real-world PDF corpus files (basic, encrypted, tagged, malformed, adversarial)
- On-demand CI workflow for testing against 1800+ external PDFs

### Feature Flags
- `jpeg2000` (default): JPEG2000 decoding via openjpeg. Disable with `default-features = false` to avoid the C build dependency.
