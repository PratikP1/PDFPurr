//! Corpus structure detection and accessibility report.
//!
//! Runs structure detection, auto-tagging, and accessibility checking
//! on every corpus file. Produces a detailed report.

use pdfpurr::content::structure_detection::BlockRole;
use pdfpurr::Document;
use std::path::Path;

#[test]
fn corpus_structure_and_accessibility_report() {
    let corpus_dir = Path::new("tests/corpus");
    let mut report = Vec::new();

    report.push("# PDFPurr Corpus Structure & Accessibility Report\n".to_string());

    let categories = [
        ("basic", "Basic PDFs"),
        ("scanned", "Scanned PDFs (image-only)"),
        ("tagged", "Tagged PDFs (PDF/UA)"),
        ("malformed", "Malformed PDFs"),
        ("encrypted", "Encrypted PDFs"),
    ];

    let mut total_files = 0;
    let mut total_parsed = 0;
    let mut total_tagged_before = 0;
    let mut total_tagged_after = 0;
    let mut total_blocks = 0;
    let mut total_headings = 0;
    let mut total_paragraphs = 0;
    let mut total_lists = 0;
    let mut total_code = 0;
    let mut total_issues = 0;

    for (subdir, title) in &categories {
        let dir = corpus_dir.join(subdir);
        if !dir.exists() {
            continue;
        }

        report.push(format!("\n## {title}\n"));

        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |e| e == "pdf"))
            .collect();
        files.sort();

        for path in &files {
            total_files += 1;
            let name = path.file_name().unwrap().to_string_lossy();
            let data = std::fs::read(path).unwrap();

            // Try to parse
            let doc = match Document::from_bytes(&data) {
                Ok(d) => d,
                Err(_) => match Document::from_bytes_with_password(&data, b"") {
                    Ok(d) => d,
                    Err(e) => {
                        report.push(format!("### {name}\n- **Parse:** FAILED ({e})\n"));
                        continue;
                    }
                },
            };
            total_parsed += 1;

            let pages = doc.page_count().unwrap_or(0);
            let has_tree = doc.structure_tree().is_some();
            if has_tree {
                total_tagged_before += 1;
            }

            // Text extraction
            let text_len: usize = (0..pages.min(3))
                .map(|i| doc.extract_page_text(i).map(|t| t.len()).unwrap_or(0))
                .sum();

            // Structure detection on first page
            let blocks = doc.analyze_page_structure(0).unwrap_or_default();
            let headings = blocks
                .iter()
                .filter(|b| matches!(b.role, BlockRole::Heading(_)))
                .count();
            let paragraphs = blocks
                .iter()
                .filter(|b| b.role == BlockRole::Paragraph)
                .count();
            let lists = blocks
                .iter()
                .filter(|b| b.role == BlockRole::ListItem)
                .count();
            let code = blocks.iter().filter(|b| b.role == BlockRole::Code).count();

            total_blocks += blocks.len();
            total_headings += headings;
            total_paragraphs += paragraphs;
            total_lists += lists;
            total_code += code;

            // Auto-tag test (on a clone to not modify original)
            let auto_tag_result = if !has_tree && !blocks.is_empty() {
                let mut doc_clone = Document::from_bytes(&data).unwrap();
                match doc_clone.auto_tag("en") {
                    Ok(n) if n > 0 => {
                        total_tagged_after += 1;
                        let tree_exists = doc_clone.structure_tree().is_some();
                        format!("Tagged {n} blocks, tree={tree_exists}")
                    }
                    Ok(_) => "No blocks to tag".to_string(),
                    Err(e) => format!("FAILED: {e}"),
                }
            } else if has_tree {
                "Already tagged".to_string()
            } else {
                "No text content".to_string()
            };

            // Accessibility check
            let issues = doc.check_accessibility();
            total_issues += issues.len();
            let error_count = issues.iter().filter(|i| i.severity == "error").count();
            let warn_count = issues.iter().filter(|i| i.severity == "warning").count();

            report.push(format!("### {name}"));
            report.push(format!(
                "- **Pages:** {pages} | **Text:** {text_len} chars | **Tagged:** {has_tree}"
            ));
            report.push(format!(
                "- **Structure:** {headings} headings, {paragraphs} paragraphs, {lists} lists, {code} code"
            ));
            report.push(format!("- **Auto-tag:** {auto_tag_result}"));
            if !issues.is_empty() {
                report.push(format!(
                    "- **Issues:** {error_count} errors, {warn_count} warnings"
                ));
                for issue in &issues {
                    report.push(format!(
                        "  - [{}] {}: {}",
                        issue.severity, issue.description, issue.suggestion
                    ));
                }
            } else {
                report.push("- **Issues:** None".to_string());
            }
            report.push(String::new());
        }
    }

    // Summary
    report.push("\n## Summary\n".to_string());
    report.push(format!("| Metric | Count |"));
    report.push(format!("|--------|-------|"));
    report.push(format!("| Files tested | {total_files} |"));
    report.push(format!("| Parsed successfully | {total_parsed} |"));
    report.push(format!(
        "| Tagged before auto-tag | {total_tagged_before} |"
    ));
    report.push(format!(
        "| Tagged after auto-tag | {} |",
        total_tagged_before + total_tagged_after
    ));
    report.push(format!("| Blocks detected | {total_blocks} |"));
    report.push(format!("| Headings | {total_headings} |"));
    report.push(format!("| Paragraphs | {total_paragraphs} |"));
    report.push(format!("| List items | {total_lists} |"));
    report.push(format!("| Code blocks | {total_code} |"));
    report.push(format!("| Accessibility issues | {total_issues} |"));

    let report_text = report.join("\n");
    eprintln!("{report_text}");

    // Write report to file
    std::fs::write("tests/CORPUS_REPORT.md", &report_text).unwrap();
    eprintln!("\nReport written to tests/CORPUS_REPORT.md");

    // Assertions: the system should work
    assert!(total_parsed > 20, "Should parse most files");
    assert!(total_blocks > 0, "Should detect some structure");
    assert!(total_headings > 0, "Should detect headings");
}
