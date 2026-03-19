//! Extracts text from all pages of a PDF.

use pdfpurr::Document;

fn main() -> pdfpurr::PdfResult<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str()).unwrap_or("input.pdf");

    let doc = Document::open(path)?;
    let page_count = doc.page_count()?;
    println!("Opened {} — {} page(s)", path, page_count);

    for i in 0..page_count {
        let text = doc.extract_page_text(i)?;
        if !text.is_empty() {
            println!("--- Page {} ---", i + 1);
            println!("{}", text);
        }
    }

    Ok(())
}
