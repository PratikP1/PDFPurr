# PDFPurr

**The ultimate pure-Rust PDF library**

[![CI](https://github.com/PratikP1/PDFPurr/actions/workflows/ci.yml/badge.svg)](https://github.com/PratikP1/PDFPurr/actions)
[![crates.io](https://img.shields.io/crates/v/pdfpurr.svg)](https://crates.io/crates/pdfpurr)
[![docs.rs](https://docs.rs/pdfpurr/badge.svg)](https://docs.rs/pdfpurr)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

PDFPurr is a comprehensive PDF library for Rust that reads, writes, edits, renders, OCRs, and validates PDF documents. It supports PDF 1.0 through 2.0, with first-class support for accessibility (PDF/UA), archival (PDF/A), and print production (PDF/X) standards.

1000+ tests across unit, integration, adversarial, property-based, and fuzz testing. CI runs on Ubuntu, macOS, and Windows with nightly clippy and libFuzzer smoke tests.

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

// Lazy loading (parse objects on demand — fast for large files)
let doc = Document::from_bytes_lazy(&data).unwrap();

// Memory-mapped file (OS pages in data on demand)
let doc = Document::open_mmap("large_report.pdf").unwrap();

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

### OCR for Image-Only PDFs

Make scanned documents searchable and accessible. Three engines available — Windows OCR and Tesseract work out of the box with no feature flags.

**Windows OCR (recommended on Windows, ~95% accuracy, zero dependencies):**
```rust
use pdfpurr::Document;
use pdfpurr::ocr::{OcrConfig, OcrEngine};
use pdfpurr::ocr::windows_engine::WindowsOcrEngine;

let engine = WindowsOcrEngine::new();
let mut doc = Document::open("scanned.pdf").unwrap();
doc.ocr_all_pages(&engine, &OcrConfig::default()).unwrap();
doc.save("searchable.pdf").unwrap();
```

**Tesseract (~85-89% accuracy, requires `tesseract` CLI):**
```rust
use pdfpurr::ocr::tesseract_engine::TesseractEngine;

let engine = TesseractEngine::new("eng");
if engine.is_available() {
    doc.ocr_all_pages(&engine, &OcrConfig::default()).unwrap();
}
```

**ocrs (pure Rust, Latin only, requires `ocr` feature):**
```rust
use pdfpurr::ocr::ocrs_engine::OcrsEngine;  // requires "ocr" feature

let engine = OcrsEngine::new("text-detection.rten", "text-recognition.rten").unwrap();
doc.ocr_all_pages(&engine, &OcrConfig::default()).unwrap();
```

All engines overlay invisible text (rendering mode 3) with tagged PDF structure (`<Document>`, `<H1>`–`<H6>`, `<P>`) for screen reader accessibility.

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
    if outline.is_bold() { println!("  [bold]"); }
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
    if let Some(uri) = &annot.uri { println!("  URI: {}", uri); }
    if let Some(author) = &annot.author { println!("  Author: {}", author); }
    if !annot.quad_points.is_empty() {
        println!("  QuadPoints: {} values", annot.quad_points.len());
    }
}
```

### Page Rendering

Render PDF pages to pixel images using the tiny-skia backend.

```rust
use pdfpurr::{Document, Renderer, RenderOptions};

let doc = Document::open("slides.pdf").unwrap();
let renderer = Renderer::new(&doc, RenderOptions {
    dpi: 150.0,
    background: [255, 255, 255, 255],
});

let pixmap = renderer.render_page(0).unwrap();
pixmap.save_png("page1.png").unwrap();
```

The renderer supports the full ISO 32000-2 content stream operator set including annotation appearance streams.

### Creating PDFs

Build new PDF documents from scratch.

```rust
use pdfpurr::Document;

let mut doc = Document::new();
doc.add_page(612.0, 792.0).unwrap(); // US Letter
doc.add_page(595.0, 842.0).unwrap(); // A4
doc.save("output.pdf").unwrap();
```

### Page Manipulation

Merge, split, reorder, rotate, and remove pages.

```rust
use pdfpurr::Document;

let mut doc = Document::open("report.pdf").unwrap();
doc.rotate_page(0, 90).unwrap();
doc.remove_page(2).unwrap();
doc.reorder_pages(&[2, 0, 1]).unwrap();

let other = Document::open("appendix.pdf").unwrap();
doc.merge(&other).unwrap();
doc.save("combined.pdf").unwrap();
```

### Font Embedding

Embed TrueType and OpenType fonts with automatic subsetting.

```rust
use pdfpurr::EmbeddedFont;

let font_data = std::fs::read("fonts/Roboto-Regular.ttf").unwrap();
let font = EmbeddedFont::from_ttf(&font_data).unwrap();
let subset = font.subset(&['H', 'e', 'l', 'o']).unwrap();

// Variable font support
let bold = EmbeddedFont::from_ttf_with_axes(&font_data, &[("wght", 700.0)]).unwrap();

// CJK text via CidFont
use pdfpurr::CidFont;
let cjk = CidFont::from_ttf(&std::fs::read("NotoSansCJK.ttf").unwrap()).unwrap();
```

### Form Fields

Read and fill AcroForm fields.

```rust
use pdfpurr::Document;

let mut doc = Document::open("form.pdf").unwrap();
for field in doc.form_fields() {
    println!("{}: {:?} = {:?}", field.name, field.field_type, field.value);
}
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
    if let Some(name) = &sig.name { println!("Signer: {}", name); }
}
```

### Standards Validation

Validate documents against PDF/A, PDF/X, and PDF/UA standards.

```rust
use pdfpurr::{Document, PdfALevel};

let doc = Document::open("archive.pdf").unwrap();
let report = doc.validate_pdfa(PdfALevel::A2b);
println!("PDF/A-2b compliant: {}", report.is_compliant());

let a11y = doc.accessibility_report();
println!("Accessible: {}", a11y.is_compliant());
```

### Linearized PDF Writing

Write PDFs optimized for progressive web display (Fast Web View).

```rust
use pdfpurr::Document;

let mut doc = Document::new();
doc.add_page(612.0, 792.0).unwrap();
let linearized = doc.to_linearized_bytes().unwrap();
```

### Incremental Updates

Append changes without rewriting the original file, preserving digital signatures.

```rust
use pdfpurr::Document;

let original = std::fs::read("signed_contract.pdf").unwrap();
let mut doc = Document::from_bytes(&original).unwrap();
doc.set_form_field("status", "approved").unwrap();
let updated = doc.to_incremental_update(&original).unwrap();
std::fs::write("approved.pdf", &updated).unwrap();
```

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `jpeg2000` | Yes | JPEG2000 decoding via openjpeg (C dependency) |
| `ocr` | No | Adds ocrs engine (pure Rust, Latin text) |

Windows OCR and Tesseract engines are **always available** — no feature flag needed.

```toml
# Default (no ocrs engine)
pdfpurr = "0.2"

# With ocrs pure-Rust engine
pdfpurr = { version = "0.2", features = ["ocr"] }
```

## Architecture

```
pdfpurr/
  src/
    core/             PDF object model, filters, compression
    parser/           Lexer, object parser, xref, repair
    content/          Content stream tokenizer and builder
    fonts/            Font parsing, encoding, embedding, subsetting, variable fonts
    images/           Image extraction and embedding
    rendering/        Page-to-pixel engine (tiny-skia), annotation rendering
    encryption/       Standard security handler (R2-R6, RC4, AES-128/256)
    forms/            AcroForms (read/write)
    signatures/       Digital signature parsing and verification
    standards/        PDF/A, PDF/X validation
    accessibility/    PDF/UA, tagged PDF, structure tree, structure builder
    structure/        Outlines, annotations, metadata
    ocr/              OCR engines, text layer, layout analysis, preprocessing
    document.rs       High-level API (open, parse, lazy, mmap, write, OCR)
    page_builder.rs   Page creation API
    error.rs          Error types
  tests/
    adversarial_tests.rs  Edge-case and malformed PDF tests (22)
    corpus_tests.rs       Real-world PDF corpus (33 files)
    external_corpus_tests.rs  veraPDF, qpdf fuzz, BFO tests
    integration.rs        Write-path roundtrip tests (25)
    proptest_fuzz.rs      Property-based fuzzing (18 targets)
  fuzz/               cargo-fuzz targets (document, content stream, object)
  benches/            Criterion benchmarks (parser, tokenizer, renderer)
```

## Testing

```bash
cargo test                       # all tests
cargo bench                      # Criterion benchmarks
cargo clippy -- -D warnings      # Lint check (clean on stable and nightly)
cargo fmt --check                # Format check
cargo doc --no-deps              # Build documentation (zero warnings)
```

CI runs automatically on every push: Ubuntu/Windows/macOS (stable), Ubuntu nightly, Criterion benchmarks, and 120 seconds of fuzz testing across 3 targets. On-demand corpus testing against 1800+ external PDFs available via GitHub Actions.

## Performance

| Operation | Time |
|-----------|------|
| Parse 14-page PDF (1 MB) | 26 ms |
| Text extraction per page | 4.7 ms |
| Render page at 150 DPI | 210 ms |
| Create new Document | < 1 us |

## Security

- Zero `panic!` or `unreachable!()` in production code
- Checked arithmetic on all image dimensions and buffer allocations
- Resource limits: 256MB decoded stream max, 1M xref rebuild cap
- Depth limits on recursive structures (page tree, outlines, inherited properties)
- Fuzz-tested: 3 cargo-fuzz + 18 proptest + 22 adversarial tests

## License

Dual-licensed under MIT or Apache-2.0 at your option.
