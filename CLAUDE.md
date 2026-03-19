# PDFPurr Development Guidelines

## Attribution

All contributions to this repository are attributed to the repository owner. Do not add Claude or any AI tool as a coauthor in commits, documentation, or code comments.

## Development Philosophy

### Red/Green TDD

Follow strict red/green Test-Driven Development:

1. **Red:** Write a failing test that defines the desired behavior
2. **Green:** Write the minimum code to make the test pass
3. **Refactor:** Clean up while keeping tests green

Every feature and bug fix starts with a test. No production code without a corresponding test.

### Elegant Code

After writing code, use `/simplify` to review for reuse, quality, and efficiency. Code should be:

- Clear and readable
- Free of duplication
- Leveraging existing patterns in the codebase

### Security-First

PDFPurr processes untrusted input. All parser code must:

- **Never panic** on any input — return `Err`, not `unwrap()`
- **Bound all allocations** — clamp user-controlled sizes before allocating
- **Limit recursion** — depth limits on all recursive structures (page tree, outlines, nested objects)
- **Validate offsets** — check bounds before indexing into byte slices
- **Avoid unsafe** — minimize unsafe blocks; document safety invariants when required
- **Cap resource usage** — MAX_DECODED_SIZE (256MB), MAX_REBUILD_OBJECTS (1M), predictor parameter clamping

### Prefer Existing Crates

Do not reimplement functionality that well-maintained crates already provide. Use the ecosystem:

- **Parsing:** `nom` for parser combinators
- **Fonts:** `skrifa` / `read-fonts` (Google Fontations) for font parsing; `allsorts` for shaping and subsetting
- **Image filters:** `flate2` (deflate), `lzw` (LZW), `fax` (CCITT Group 3/4), `jpeg-decoder` (JPEG), `png` (PNG), `jpeg2k` (JPEG2000)
- **Cryptography:** `aes`, `sha2`, `md5` from RustCrypto
- **Encoding:** `encoding_rs`, `unicode-normalization`
- **2D graphics:** `tiny-skia` for rendering
- **XML:** `roxmltree` for XMP metadata
- **Memory mapping:** `memmap2` for large file support

Only write custom implementations when no suitable crate exists or when PDF-specific requirements demand it.

## Code Style

- Rust 2021 edition
- `#![deny(missing_docs)]` on all public items
- `#![warn(clippy::all)]` enabled
- Run `cargo fmt` before committing
- Run `cargo clippy -- -D warnings` and resolve all warnings (stable + nightly)
- Run `cargo doc --no-deps` and resolve all warnings

## Current Stats

- 954+ tests (unit, adversarial, corpus, integration, proptest, doc)
- CI: Ubuntu/Windows/macOS (stable) + nightly + fuzz + benchmarks
- On-demand corpus test: 1800+ external PDFs from veraPDF, qpdf, BFO
- 3 cargo-fuzz targets + 18 proptest fuzz targets + 22 adversarial tests
- Two pure-Rust OCR engines: ocrs (Latin) and PaddleOCR via tract-onnx (multi-language)
- Feature flags: `jpeg2000`, `ocr`, `ocr-paddle`

## License

Dual-licensed: MIT OR Apache-2.0. All dependencies must be compatible with this licensing.

## Build and Test

```bash
cargo test              # Run all tests (must pass before committing)
cargo clippy -- -D warnings  # Lint check (must be clean on stable + nightly)
cargo fmt --check       # Format check
cargo doc --no-deps     # Build docs (must be warning-free)
cargo bench             # Run benchmarks (for performance-sensitive changes)
```

## CI Workflows

- **ci.yml** — Runs on every push/PR: fmt, clippy, tests, docs across 3 OS + nightly + fuzz smoke tests + benchmarks
- **corpus-test.yml** — On-demand: downloads and tests hundreds of external PDFs (trigger via GitHub Actions UI)
