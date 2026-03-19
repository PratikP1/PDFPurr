//! Criterion benchmarks for PDFPurr's core operations.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pdfpurr::content::tokenize_content_stream;
use pdfpurr::parser::objects::parse_object;
use pdfpurr::rendering::{RenderOptions, Renderer};

// ---------------------------------------------------------------------------
// Parser benchmarks
// ---------------------------------------------------------------------------

fn bench_parse_integer(c: &mut Criterion) {
    c.bench_function("parse_integer", |b| {
        let input = b"12345 ";
        b.iter(|| parse_object(black_box(input)).unwrap());
    });
}

fn bench_parse_real(c: &mut Criterion) {
    c.bench_function("parse_real", |b| {
        let input = b"3.14159 ";
        b.iter(|| parse_object(black_box(input)).unwrap());
    });
}

fn bench_parse_name(c: &mut Criterion) {
    c.bench_function("parse_name", |b| {
        let input = b"/DeviceRGB ";
        b.iter(|| parse_object(black_box(input)).unwrap());
    });
}

fn bench_parse_string_literal(c: &mut Criterion) {
    c.bench_function("parse_string_literal", |b| {
        let input = b"(Hello, World!) ";
        b.iter(|| parse_object(black_box(input)).unwrap());
    });
}

fn bench_parse_hex_string(c: &mut Criterion) {
    c.bench_function("parse_hex_string", |b| {
        let input = b"<48656C6C6F> ";
        b.iter(|| parse_object(black_box(input)).unwrap());
    });
}

fn bench_parse_array(c: &mut Criterion) {
    c.bench_function("parse_array_10_ints", |b| {
        let input = b"[1 2 3 4 5 6 7 8 9 10] ";
        b.iter(|| parse_object(black_box(input)).unwrap());
    });
}

fn bench_parse_dictionary(c: &mut Criterion) {
    c.bench_function("parse_dictionary_small", |b| {
        let input = b"<< /Type /Page /Width 612 /Height 792 >> ";
        b.iter(|| parse_object(black_box(input)).unwrap());
    });
}

fn bench_parse_large_dictionary(c: &mut Criterion) {
    c.bench_function("parse_dictionary_large", |b| {
        let input = b"<< /Type /Catalog /Pages 1 0 R /Outlines 2 0 R \
                        /PageMode /UseNone /PageLayout /SinglePage \
                        /ViewerPreferences << /HideToolbar true /HideMenubar false >> \
                        /Metadata 3 0 R /MarkInfo << /Marked true >> \
                        /Lang (en-US) >> ";
        b.iter(|| parse_object(black_box(input)).unwrap());
    });
}

// ---------------------------------------------------------------------------
// Content stream benchmarks
// ---------------------------------------------------------------------------

fn bench_tokenize_simple_content(c: &mut Criterion) {
    c.bench_function("tokenize_simple_content", |b| {
        let content = b"q 1 0 0 1 72 720 cm BT /F1 12 Tf (Hello) Tj ET Q";
        b.iter(|| tokenize_content_stream(black_box(content)).unwrap());
    });
}

fn bench_tokenize_path_heavy(c: &mut Criterion) {
    c.bench_function("tokenize_path_heavy", |b| {
        // Simulate a complex path with many segments
        let mut content = Vec::new();
        content.extend_from_slice(b"q ");
        for i in 0..100 {
            let x = i as f64 * 5.0;
            let y = (i as f64 * 3.0).sin() * 100.0 + 400.0;
            let segment = format!("{:.2} {:.2} {} ", x, y, if i == 0 { "m" } else { "l" });
            content.extend_from_slice(segment.as_bytes());
        }
        content.extend_from_slice(b"S Q");

        b.iter(|| tokenize_content_stream(black_box(&content)).unwrap());
    });
}

fn bench_tokenize_text_heavy(c: &mut Criterion) {
    c.bench_function("tokenize_text_heavy", |b| {
        // Text-heavy content stream: multiple lines of text
        let mut content = Vec::new();
        content.extend_from_slice(b"BT /F1 10 Tf 72 700 Td 12 TL ");
        for _ in 0..50 {
            content.extend_from_slice(b"(The quick brown fox jumps over the lazy dog.) ' ");
        }
        content.extend_from_slice(b"ET");

        b.iter(|| tokenize_content_stream(black_box(&content)).unwrap());
    });
}

// ---------------------------------------------------------------------------
// Document construction benchmark
// ---------------------------------------------------------------------------

fn bench_document_new(c: &mut Criterion) {
    c.bench_function("document_new", |b| {
        b.iter(|| {
            black_box(pdfpurr::Document::new());
        });
    });
}

// ---------------------------------------------------------------------------
// Rendering benchmarks (synthetic — no real PDF required)
// ---------------------------------------------------------------------------

fn bench_render_blank_page(c: &mut Criterion) {
    c.bench_function("render_blank_page", |b| {
        // Build a minimal valid PDF in memory
        let pdf = build_minimal_pdf(b"");
        let doc = pdfpurr::Document::from_bytes(&pdf).unwrap();
        let renderer = Renderer::new(&doc, RenderOptions::default());

        b.iter(|| {
            let _ = black_box(renderer.render_page(black_box(0)));
        });
    });
}

fn bench_render_simple_graphics(c: &mut Criterion) {
    c.bench_function("render_simple_graphics", |b| {
        let content = b"q 1 0 0 RG 0.5 w 72 72 m 540 720 l S Q";
        let pdf = build_minimal_pdf(content);
        let doc = pdfpurr::Document::from_bytes(&pdf).unwrap();
        let renderer = Renderer::new(&doc, RenderOptions::default());

        b.iter(|| {
            let _ = black_box(renderer.render_page(black_box(0)));
        });
    });
}

/// Builds a minimal valid PDF byte stream with optional page content.
fn build_minimal_pdf(content: &[u8]) -> Vec<u8> {
    let mut pdf = Vec::new();

    // Header
    pdf.extend_from_slice(b"%PDF-1.4\n");

    // Object 1: Catalog
    let obj1_offset = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages
    let obj2_offset = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    // Object 3: Page
    let obj3_offset = pdf.len();
    if content.is_empty() {
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
    } else {
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
        );

        // Object 4: Content stream
        let obj4_offset = pdf.len();
        let stream_header = format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len());
        pdf.extend_from_slice(stream_header.as_bytes());
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        // Build xref with 5 entries
        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n0 5\n");
        pdf.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj4_offset).as_bytes());

        // Trailer
        pdf.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n");
        pdf.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
        pdf.extend_from_slice(b"%%EOF\n");
        return pdf;
    }

    // Build xref with 4 entries (no content stream)
    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 4\n");
    pdf.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_offset).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_offset).as_bytes());

    // Trailer
    pdf.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n");
    pdf.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
    pdf.extend_from_slice(b"%%EOF\n");

    pdf
}

criterion_group!(
    parser_benches,
    bench_parse_integer,
    bench_parse_real,
    bench_parse_name,
    bench_parse_string_literal,
    bench_parse_hex_string,
    bench_parse_array,
    bench_parse_dictionary,
    bench_parse_large_dictionary
);

criterion_group!(
    content_benches,
    bench_tokenize_simple_content,
    bench_tokenize_path_heavy,
    bench_tokenize_text_heavy
);

criterion_group!(
    doc_benches,
    bench_document_new,
    bench_render_blank_page,
    bench_render_simple_graphics
);

criterion_main!(parser_benches, content_benches, doc_benches);
