# PDFPurr

**The ultimate pure-Rust PDF library**

[![CI](https://github.com/PratikP1/Research-Private/actions/workflows/ci.yml/badge.svg)](https://github.com/PratikP1/Research-Private/actions)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021+-orange.svg)](https://www.rust-lang.org)

PDFPurr is a comprehensive PDF library for Rust that reads, writes, edits, renders, and validates PDF documents. It supports PDF 1.0 through 2.0, with first-class support for accessibility (PDF/UA), archival (PDF/A), and print production (PDF/X) standards.

870+ tests across unit, integration, property-based, and fuzz testing. CI runs on Ubuntu, macOS, and Windows with nightly clippy and libFuzzer smoke tests.

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
pdfpurr = "0.1"
```

```rust
use pdfpurr::Document;

// Create a new PDF
let mut doc = Document::new();
doc.add_page(612.0, 792.0).unwrap(); // US Letter
let bytes = doc.to_bytes().unwrap();

// Parse it back
let doc = Document::from_bytes(&bytes).unwrap();
assert_eq!(doc.page_count().unwrap(), 1);
```

## Feature Guide

### Reading and Parsing

PDFPurr parses any PDF from 1.0 to 2.0, including encrypted and malformed files.

```rust
use pdfpurr::Document;

// Open from disk
let doc = Document::open("report.pdf").unwrap();

// Parse from bytes
let data = std::fs::read("report.pdf").unwrap();
let doc = Document::from_bytes(&data).unwrap();

// Open an encrypted PDF
let doc = Document::from_bytes_with_password(&data, b"secret").unwrap();

// Basic document info
println!("Pages: {}", doc.page_count().unwrap());
println!("Objects: {}", doc.object_count());
if let Some(title) = doc.title() {
    println!("Title: {}", title);
}
```

**Supported encryption:** Standard security handler revisions R2 through R6 (RC4 40/128-bit, AES-128, AES-256).

**Repair:** If the cross-reference table is corrupt, PDFPurr automatically rebuilds it by scanning for object definitions. Streams with incorrect `/Length` values are recovered by scanning for `endstream`.

### Text Extraction

Extract text from individual pages or the entire document.

```rust
use pdfpurr::Document;

let doc = Document::open("paper.pdf").unwrap();

// Single page
let text = doc.extract_page_text(0).unwrap();
println!("Page 1: {}", text);

// All pages
let all_text = doc.extract_all_text().unwrap();
println!("{}", all_text);
```

Text extraction uses font encoding tables (WinAnsiEncoding, MacRomanEncoding, PDFDocEncoding) and ToUnicode CMaps for accurate character mapping, including CJK scripts.

### Image Extraction

Extract images from any page.

```rust
use pdfpurr::Document;

let doc = Document::open("brochure.pdf").unwrap();

// All images across all pages
let images = doc.extract_all_images().unwrap();
for (page_idx, name, image) in &images {
    println!(
        "Page {}: {} ({}x{}, {:?})",
        page_idx, name, image.width, image.height, image.color_space
    );
}

// Images from a specific page
let page = doc.get_page(0).unwrap();
let page_images = doc.page_images(page);
```

Supported filters: FlateDecode, DCTDecode (JPEG), JPXDecode (JPEG2000), CCITTFaxDecode, LZWDecode, ASCIIHexDecode, ASCII85Decode, RunLengthDecode.

### Metadata

Read document metadata from both the Info dictionary and XMP streams.

```rust
use pdfpurr::Document;

let doc = Document::open("report.pdf").unwrap();
let meta = doc.metadata();

if let Some(title) = &meta.title { println!("Title: {}", title); }
if let Some(author) = &meta.author { println!("Author: {}", author); }
if let Some(subject) = &meta.subject { println!("Subject: {}", subject); }
if let Some(creator) = &meta.creator { println!("Creator: {}", creator); }
if let Some(producer) = &meta.producer { println!("Producer: {}", producer); }
if let Some(date) = &meta.creation_date { println!("Created: {}", date); }
```

XMP metadata is parsed with namespace-aware XML processing, supporting `rdf:Alt`/`rdf:Seq` containers and `rdf:Description` attribute forms.

### Outlines (Bookmarks)

Read the document outline tree, including actions and styling.

```rust
use pdfpurr::Document;

let doc = Document::open("book.pdf").unwrap();
let outlines = doc.outlines();

for outline in &outlines {
    println!("{}", outline.title);
    if let Some(uri) = &outline.uri {
        println!("  Link: {}", uri);
    }
    if let Some(action) = &outline.action_type {
        println!("  Action: {}", action);
    }
    if outline.is_bold() { println!("  [bold]"); }
    if outline.is_italic() { println!("  [italic]"); }
    for child in &outline.children {
        println!("  - {}", child.title);
    }
}
```

### Annotations

Extract annotations from pages with full metadata.

```rust
use pdfpurr::Document;

let doc = Document::open("annotated.pdf").unwrap();
let page = doc.get_page(0).unwrap();
let annotations = doc.page_annotations(page);

for annot in &annotations {
    println!("{} at {:?}", annot.subtype, annot.rect);
    if let Some(uri) = &annot.uri {
        println!("  URI: {}", uri);
    }
    if let Some(author) = &annot.author {
        println!("  Author: {}", author);
    }
    if annot.is_hidden() { println!("  [hidden]"); }
    if !annot.quad_points.is_empty() {
        println!("  QuadPoints: {} values", annot.quad_points.len());
    }
}
```

Supported annotation properties: subtype, rect, contents, flags (Hidden/Print/ReadOnly), color, author, modification date, URI (Link), QuadPoints (Highlight/Underline/StrikeOut).

### Page Rendering

Render PDF pages to pixel images using the tiny-skia backend.

```rust
use pdfpurr::{Document, Renderer, RenderOptions};

let doc = Document::open("slides.pdf").unwrap();
let renderer = Renderer::new(&doc, RenderOptions {
    dpi: 150.0,
    background: [255, 255, 255, 255], // white
});

let pixmap = renderer.render_page(0).unwrap();
pixmap.save_png("page1.png").unwrap();

// Or use the convenience method on Document
let pixmap = doc.render_page(0, 72.0).unwrap();
```

The renderer supports the full ISO 32000-2 content stream operator set: paths, text (with embedded fonts), images, shading patterns, tiling patterns, transparency groups, soft masks, blend modes, clipping, and annotation overlays (Link and Highlight).

### Creating PDFs

Build new PDF documents from scratch.

```rust
use pdfpurr::Document;

let mut doc = Document::new();

// Add pages with different sizes
doc.add_page(612.0, 792.0).unwrap(); // US Letter
doc.add_page(595.0, 842.0).unwrap(); // A4

// Save to disk
doc.save("output.pdf").unwrap();

// Or get bytes in memory
let bytes = doc.to_bytes().unwrap();
```

### Page Manipulation

Merge, split, reorder, rotate, and remove pages.

```rust
use pdfpurr::Document;

let mut doc = Document::open("report.pdf").unwrap();

// Rotate a page
doc.rotate_page(0, 90).unwrap();

// Remove a page
doc.remove_page(2).unwrap();

// Reorder pages
doc.reorder_pages(&[2, 0, 1]).unwrap();

// Merge another PDF
let other = Document::open("appendix.pdf").unwrap();
doc.merge(&other).unwrap();

doc.save("combined.pdf").unwrap();
```

### Font Embedding

Embed TrueType and OpenType fonts with automatic subsetting.

```rust
use pdfpurr::EmbeddedFont;

// Load and subset a TTF font
let font_data = std::fs::read("fonts/Roboto-Regular.ttf").unwrap();
let font = EmbeddedFont::from_ttf(&font_data).unwrap();
let subset = font.subset(&['H', 'e', 'l', 'o', ' ', 'W', 'r', 'd']).unwrap();

// Measure text width
let width = font.measure_text("Hello World", 12.0).unwrap();
println!("Width at 12pt: {:.1}pt", width);

// For CJK text, use CidFont (supports >256 glyphs)
use pdfpurr::CidFont;
let cjk_data = std::fs::read("fonts/NotoSansCJK.ttf").unwrap();
let cjk_font = CidFont::from_ttf(&cjk_data).unwrap();
let cjk_subset = cjk_font.subset(&"Hello".chars().collect::<Vec<_>>()).unwrap();
```

### Form Fields

Read and fill AcroForm fields.

```rust
use pdfpurr::Document;

let mut doc = Document::open("form.pdf").unwrap();

// Read existing fields
for field in doc.form_fields() {
    println!("{}: {:?} = {:?}", field.name, field.field_type, field.value);
}

// Set a field value
doc.set_form_field("username", "alice").unwrap();
doc.save("filled_form.pdf").unwrap();
```

### Digital Signatures

Parse and verify digital signature integrity.

```rust
use pdfpurr::Document;

let doc = Document::open("signed.pdf").unwrap();
for sig in doc.signatures() {
    println!("Filter: {}", sig.filter);
    println!("Sub-filter: {:?}", sig.sub_filter);
    println!("Byte range: {:?}", sig.byte_range);
    if let Some(name) = &sig.name { println!("Signer: {}", name); }
    if let Some(date) = &sig.signing_date { println!("Date: {}", date); }
}
```

### Standards Validation

Validate documents against PDF/A, PDF/X, and PDF/UA standards.

```rust
use pdfpurr::{Document, PdfALevel};

let doc = Document::open("archive.pdf").unwrap();

// PDF/A validation
let report = doc.validate_pdfa(PdfALevel::A2b);
println!("PDF/A-2b compliant: {}", report.is_compliant());
for check in &report.checks {
    if !check.passed {
        println!("  FAIL: {} - {}", check.id, check.description);
    }
}

// PDF/UA accessibility validation
let a11y = doc.accessibility_report();
println!("Accessible: {}", a11y.is_compliant());

// Structure tree inspection
if let Some(tree) = doc.structure_tree() {
    println!("Root tag: {:?}", tree.root_elements().first().map(|e| &e.role));
}
```

### Linearized PDF Writing

Write PDFs optimized for progressive web display (Fast Web View).

```rust
use pdfpurr::Document;

let mut doc = Document::new();
doc.add_page(612.0, 792.0).unwrap();

// Standard output
let bytes = doc.to_bytes().unwrap();

// Linearized output (first page loads faster)
let linearized = doc.to_linearized_bytes().unwrap();
std::fs::write("fast_web_view.pdf", &linearized).unwrap();
```

### Incremental Updates

Append changes without rewriting the original file, preserving digital signatures.

```rust
use pdfpurr::Document;

let original = std::fs::read("signed_contract.pdf").unwrap();
let mut doc = Document::from_bytes(&original).unwrap();

// Make changes
doc.set_form_field("status", "approved").unwrap();

// Write as incremental update (original bytes preserved)
let updated = doc.to_incremental_update(&original).unwrap();
std::fs::write("signed_contract_approved.pdf", &updated).unwrap();
```

## Architecture

```
pdfpurr/
  src/
    core/             PDF object model (Object, Dictionary, PdfStream, filters)
    parser/           Lexer, object parser, file structure, xref rebuild
    content/          Content stream tokenizer and builder
    fonts/            Font parsing, encoding, embedding, subsetting, Standard 14
    images/           Image extraction and embedding
    rendering/        Page-to-pixel engine (tiny-skia backend)
    encryption/       Standard security handler (R2-R6, RC4, AES-128/256)
    forms/            AcroForms (read/write)
    signatures/       Digital signature parsing and verification
    standards/        PDF/A, PDF/X validation
    accessibility/    PDF/UA, tagged PDF, structure tree
    structure/        Outlines, annotations, metadata
    document.rs       High-level Document API
    page_builder.rs   Page creation API
    error.rs          Error types
  tests/
    corpus_tests.rs   Real-world PDF corpus (26 files)
    integration.rs    Write-path roundtrip tests
    proptest_fuzz.rs  Property-based fuzzing (14 targets)
  fuzz/               cargo-fuzz targets (document, content stream, object)
  benches/            Criterion benchmarks (parser, tokenizer, renderer)
```

## Testing

```bash
cargo test            # 870 tests (unit + integration + proptest + doc-tests)
cargo bench           # Criterion benchmarks
cargo clippy          # Lint check (clean on stable and nightly)
cargo fmt --check     # Format check
cargo doc --no-deps   # Build documentation (zero warnings)
```

CI runs automatically on every push: Ubuntu/Windows/macOS (stable), Ubuntu nightly, Criterion benchmarks, and 120 seconds of fuzz testing across 3 targets.

## Performance

Measured on a standard developer machine:

| Operation | Time |
|-----------|------|
| Parse 14-page PDF (1 MB) | 26 ms |
| Text extraction per page | 4.7 ms |
| Render page at 150 DPI | 210 ms |
| Create new Document | < 1 us |

## License

Dual-licensed under MIT or Apache-2.0 at your option.
