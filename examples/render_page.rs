//! Renders the first page of a PDF to a PNG image.

use pdfpurr::rendering::{RenderOptions, Renderer};
use pdfpurr::Document;

fn main() -> pdfpurr::PdfResult<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str()).unwrap_or("input.pdf");

    let doc = Document::open(path)?;
    println!(
        "Opened {} — {} page(s)",
        path,
        doc.page_count().unwrap_or(0)
    );

    let opts = RenderOptions {
        dpi: 150.0,
        ..Default::default()
    };
    let renderer = Renderer::new(&doc, opts);
    let pixmap = renderer.render_page(0)?;

    let out = format!("{}.png", path.trim_end_matches(".pdf"));
    pixmap
        .save_png(&out)
        .map_err(|e| pdfpurr::PdfError::Other(format!("PNG save: {}", e)))?;
    println!(
        "Rendered page 1 to {} ({}x{} px)",
        out,
        pixmap.width(),
        pixmap.height()
    );

    Ok(())
}
