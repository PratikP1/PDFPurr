# PDFPurr

**A pure-Rust PDF library**

[![CI](https://github.com/PratikP1/PDFPurr/actions/workflows/ci.yml/badge.svg)](https://github.com/PratikP1/PDFPurr/actions)
[![crates.io](https://img.shields.io/crates/v/pdfpurr.svg)](https://crates.io/crates/pdfpurr)
[![docs.rs](https://docs.rs/pdfpurr/badge.svg)](https://docs.rs/pdfpurr)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

PDFPurr reads, writes, edits, renders, OCRs, and validates PDF documents in Rust. It handles PDF 1.0 through 2.0 with PDF/UA (accessibility), PDF/A (archival), and PDF/X (print production) standards.

1150+ tests. CI on Ubuntu, macOS, and Windows with nightly clippy and libFuzzer smoke tests.

## Quick Start

```toml
[dependencies]
pdfpurr = "0.4"
```

```rust
use pdfpurr::Document;

let mut doc = Document::new();
doc.add_page(612.0, 792.0).unwrap(); // US Letter
let bytes = doc.to_bytes().unwrap();

let doc = Document::from_bytes(&bytes).unwrap();
assert_eq!(doc.page_count().unwrap(), 1);
```

## Feature Guide

### Reading and Parsing

Parses any PDF from 1.0 to 2.0, including encrypted and malformed files.

```rust
use pdfpurr::Document;

let doc = Document::open("report.pdf").unwrap();
let doc = Document::from_bytes(&data).unwrap();
let doc = Document::from_bytes_with_password(&data, b"secret").unwrap();
let doc = Document::from_bytes_lazy(&data).unwrap();     // parse on demand
let doc = Document::open_mmap("large.pdf").unwrap();      // memory-mapped
```

**Encryption:** R2 through R6 (RC4 40/128-bit, AES-128, AES-256). Constant-time password comparison.

**Repair:** Rebuilds corrupt xref tables by scanning for object definitions. Recovers streams with wrong `/Length` via `endstream` scanning. Falls back to raw deflate when zlib headers are corrupt.

### Text Extraction

```rust
let text = doc.extract_page_text(0).unwrap();
let all_text = doc.extract_all_text().unwrap();
```

Uses font encoding tables (WinAnsi, MacRoman, PDFDoc) and ToUnicode CMaps. CJK scripts supported.

### Structure Detection

Detects headings, paragraphs, lists, tables, and code blocks from font metrics and text positions — no OCR needed for native text.

```rust
use pdfpurr::content::structure_detection::BlockRole;

let blocks = doc.analyze_page_structure(0).unwrap();
for block in &blocks {
    match &block.role {
        BlockRole::Heading(level) => println!("H{level}: {}", block.runs[0].text),
        BlockRole::Paragraph => println!("P: {} chars", block.runs.len()),
        BlockRole::ListItem => println!("LI: {}", block.runs[0].text),
        BlockRole::Code => println!("Code block"),
        _ => {}
    }
}
```

Also detects tables (column alignment), headers/footers (repeated across pages), form field labels (proximity), and figure captions.

### Auto-Tagging

Adds a PDF/UA structure tree to untagged PDFs based on detected content structure.

```rust
let blocks_tagged = doc.auto_tag("en-US").unwrap();
println!("Tagged {} blocks", blocks_tagged);
```

### Accessibility Checking

Reports issues by comparing detected structure against existing tags.

```rust
let issues = doc.check_accessibility();
for issue in &issues {
    println!("[{}] {}: {}", issue.severity, issue.description, issue.suggestion);
}
```

Detects: untagged documents, missing language, heading count mismatches, missing alt text on figures, heading level skips (H1 → H3 without H2).

### OCR for Image-Only PDFs

Three engines. Windows OCR and Tesseract need no feature flags.

```rust
// Windows OCR (~95% accuracy, zero dependencies)
use pdfpurr::ocr::windows_engine::WindowsOcrEngine;
let engine = WindowsOcrEngine::new();
doc.ocr_all_pages(&engine, &OcrConfig::default()).unwrap();

// Tesseract (~85-89%, requires tesseract CLI)
use pdfpurr::ocr::tesseract_engine::TesseractEngine;
let engine = TesseractEngine::new("eng");

// ocrs (pure Rust, Latin only, requires "ocr" feature)
use pdfpurr::ocr::ocrs_engine::OcrsEngine;
let engine = OcrsEngine::new("det.rten", "rec.rten").unwrap();
```

Invisible text overlay (rendering mode 3) with tagged structure for screen readers. Full Unicode via ToUnicode CMap — CJK, emoji, and symbols preserved.

### Hybrid OCR + Text Comparison

Compares content stream text against OCR output. When they disagree, presents both to screen readers.

```rust
let result = doc.hybrid_ocr_page(0, &engine, &config).unwrap();
match result.source {
    TextSource::ContentStream => println!("Text is reliable"),
    TextSource::Ocr => println!("OCR is better"),
    TextSource::Both => println!("Disagreement: {}", result.accessible_text),
    TextSource::Neither => println!("No text found"),
}
```

### Text Run Analysis

Extracts positioned text runs with font name, size, position, color, and style flags.

```rust
let runs = doc.extract_text_runs(0).unwrap();
for run in &runs {
    println!("{} at ({:.0}, {:.0}) {}pt {}{}",
        run.text, run.x, run.y, run.font_size,
        if run.is_bold { "bold " } else { "" },
        if run.is_italic { "italic" } else { "" },
    );
}
```

### Image Extraction

```rust
let images = doc.extract_all_images().unwrap();
for (page, name, img) in &images {
    println!("Page {page}: {name} ({}x{})", img.width, img.height);
}
```

Extracts both XObject images and inline images (BI/ID/EI). Filters: FlateDecode, DCT, JPX, CCITT, LZW, ASCII85, ASCIIHex, RunLength.

### Metadata

```rust
let meta = doc.metadata();
if let Some(title) = &meta.title { println!("Title: {title}"); }
```

Reads Info dictionary and XMP streams.

### Outlines, Annotations, Form Fields, Signatures

```rust
let outlines = doc.outlines();
let annots = doc.page_annotations(page);
let fields = doc.form_fields();
let sigs = doc.signatures();
```

### Page Rendering

```rust
use pdfpurr::{Renderer, RenderOptions};
let renderer = Renderer::new(&doc, RenderOptions { dpi: 150.0, ..Default::default() });
let pixmap = renderer.render_page(0).unwrap();
```

### Creating and Manipulating PDFs

```rust
let mut doc = Document::new();
doc.add_page(612.0, 792.0).unwrap();
doc.rotate_page(0, 90).unwrap();
doc.merge(&other_doc).unwrap();
doc.save("output.pdf").unwrap();
```

### Font Embedding

TTF, OTF/CFF, and variable fonts with automatic subsetting.

```rust
let font = EmbeddedFont::from_ttf(&data).unwrap();
let subset = font.subset(&['H', 'e', 'l', 'o']).unwrap();

let otf = EmbeddedFont::from_otf(&otf_data).unwrap(); // CFF outlines
let bold = EmbeddedFont::from_ttf_with_axes(&data, &[("wght", 700.0)]).unwrap();
```

### Standards Validation

```rust
let report = doc.validate_pdfa(PdfALevel::A2b);
let a11y = doc.accessibility_report();
```

### Linearized Writing and Incremental Updates

```rust
let linearized = doc.to_linearized_bytes().unwrap();
let incremental = doc.to_incremental_update(&original_bytes).unwrap();
```

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `jpeg2000` | Yes | JPEG2000 decoding (C dependency via openjpeg) |
| `ocr` | No | Adds ocrs engine (pure Rust, Latin text) |
| `ocr-windows-native` | No | Native Windows OCR via WinRT (Windows 10+) |

Windows OCR (subprocess) and Tesseract are always available.

```toml
pdfpurr = "0.4"
pdfpurr = { version = "0.4", features = ["ocr"] }
```

## Architecture

```
src/
  core/               Object model, filters, compression
  parser/             Lexer, object parser, xref, repair
  content/            Tokenizer, builder, text analysis, structure detection
  fonts/              Parsing, encoding, embedding, subsetting
  images/             Extraction and embedding (XObject + inline)
  rendering/          Page-to-pixel (tiny-skia), annotation rendering
  encryption/         R2-R6, RC4, AES-128/256, constant-time comparison
  forms/              AcroForms (read/write)
  signatures/         Digital signature parsing
  standards/          PDF/A, PDF/X validation
  accessibility/      PDF/UA, structure tree, auto-tagging, quality checks
  structure/          Outlines, annotations, metadata
  ocr/                OCR engines, text layer, hybrid comparison
  document.rs         High-level API
```

## Testing

```bash
cargo test            # 1150+ tests
cargo bench           # Criterion benchmarks
cargo clippy          # Lint (clean on stable + nightly)
cargo doc --no-deps   # Docs (zero warnings)
```

CI: Ubuntu/Windows/macOS (stable) + nightly + fuzz + benchmarks. On-demand: 1800+ external PDFs from veraPDF, qpdf, BFO.

## Performance

| Operation | Time |
|-----------|------|
| Parse 14-page PDF (1 MB) | 26 ms |
| Text extraction per page | 4.7 ms |
| Render page at 150 DPI | 210 ms |

## Security

- Zero `panic!` or `unreachable!()` in production code
- Constant-time password hash comparison
- Checked arithmetic on image dimensions and buffer allocations
- Resource limits: 256 MB decoded stream max, 1M xref rebuild cap
- Depth limits: page tree (64), outlines (32), inherited properties (32)
- Cycle detection on outline /Next chains
- Raw deflate fallback for corrupt zlib headers
- Fuzz-tested: 3 cargo-fuzz + 18 proptest + 22 adversarial tests

## License

Dual-licensed under MIT or Apache-2.0 at your option.
