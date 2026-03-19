//! Creates a simple PDF document and saves it to disk.

use pdfpurr::Document;

fn main() -> pdfpurr::PdfResult<()> {
    let mut doc = Document::new();

    // Add a US Letter page (612 x 792 points)
    doc.add_page(612.0, 792.0)?;

    // Serialize to bytes
    let bytes = doc.to_bytes()?;
    println!("Created PDF: {} bytes, {} page(s)", bytes.len(), 1);

    // Save to file
    doc.save("output.pdf")?;
    println!("Saved to output.pdf");

    Ok(())
}
