//! PDF/UA accessibility validation.
//!
//! Implements a subset of the Matterhorn Protocol checks for PDF/UA-1
//! (ISO 14289-1:2014) compliance.

use super::structure_tree::{StandardRole, StructElem, StructTree};

/// An individual accessibility check result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilityCheck {
    /// Short identifier for the check.
    pub id: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// Whether the check passed.
    pub passed: bool,
    /// Details about failures (empty if passed).
    pub details: Vec<String>,
}

/// An accessibility validation report for a PDF document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilityReport {
    /// The individual check results.
    pub checks: Vec<AccessibilityCheck>,
}

impl AccessibilityReport {
    /// Returns `true` if all checks passed.
    pub fn is_compliant(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    /// Returns only the failed checks.
    pub fn failures(&self) -> Vec<&AccessibilityCheck> {
        self.checks.iter().filter(|c| !c.passed).collect()
    }

    /// Returns the total number of checks.
    pub fn total_checks(&self) -> usize {
        self.checks.len()
    }

    /// Returns the number of passed checks.
    pub fn passed_count(&self) -> usize {
        self.checks.iter().filter(|c| c.passed).count()
    }
}

/// Validates a PDF structure tree against PDF/UA accessibility requirements.
///
/// Performs the following checks:
/// 1. **Tagged PDF**: The document has a non-empty structure tree
/// 2. **Document language**: `/Lang` is set on the structure tree root
/// 3. **Figure alt text**: All `Figure` elements have `/Alt` text
/// 4. **Heading order**: Headings are properly nested (no skipping levels)
/// 5. **Table headers**: Tables contain at least one `TH` element
pub fn validate_pdf_ua(tree: &StructTree) -> AccessibilityReport {
    let checks = vec![
        check_tagged_pdf(tree),
        check_document_language(tree),
        check_figure_alt_text(tree),
        check_heading_order(tree),
        check_table_headers(tree),
        check_pdfua2_fenote(tree),
    ];

    AccessibilityReport { checks }
}

/// Check 1: Document must be tagged (non-empty structure tree).
fn check_tagged_pdf(tree: &StructTree) -> AccessibilityCheck {
    AccessibilityCheck {
        id: "tagged-pdf",
        description: "Document must be a tagged PDF with structure tree",
        passed: !tree.children.is_empty(),
        details: if tree.children.is_empty() {
            vec!["Structure tree has no children".into()]
        } else {
            vec![]
        },
    }
}

/// Check 2: Document must have a language specified.
fn check_document_language(tree: &StructTree) -> AccessibilityCheck {
    AccessibilityCheck {
        id: "document-language",
        description: "Document must specify a natural language (/Lang)",
        passed: tree.lang.is_some(),
        details: if tree.lang.is_none() {
            vec!["No /Lang entry on StructTreeRoot".into()]
        } else {
            vec![]
        },
    }
}

/// Check 3: All Figure elements must have alternative text.
fn check_figure_alt_text(tree: &StructTree) -> AccessibilityCheck {
    let missing: Vec<String> = tree
        .figures_without_alt_text()
        .iter()
        .enumerate()
        .map(|(i, _)| format!("Figure #{} missing /Alt text", i + 1))
        .collect();

    AccessibilityCheck {
        id: "figure-alt-text",
        description: "All Figure elements must have alternative text (/Alt)",
        passed: missing.is_empty(),
        details: missing,
    }
}

/// Check 4: Headings must be properly nested (no skipping levels).
///
/// For example, H1 → H3 (skipping H2) is a failure.
fn check_heading_order(tree: &StructTree) -> AccessibilityCheck {
    let elements = tree.iter_elements();
    let headings: Vec<u8> = elements
        .iter()
        .filter_map(|e| match &e.role {
            StandardRole::H(level) if *level > 0 => Some(*level),
            _ => None,
        })
        .collect();

    let mut details = Vec::new();
    let mut max_level_seen: u8 = 0;

    for &level in &headings {
        if level > max_level_seen + 1 && max_level_seen > 0 {
            details.push(format!(
                "H{} follows H{} (skips level {})",
                level,
                max_level_seen,
                max_level_seen + 1
            ));
        }
        if level > max_level_seen {
            max_level_seen = level;
        }
    }

    AccessibilityCheck {
        id: "heading-order",
        description: "Headings must be properly nested without skipping levels",
        passed: details.is_empty(),
        details,
    }
}

/// Check 5: Tables must have header cells (TH).
fn check_table_headers(tree: &StructTree) -> AccessibilityCheck {
    let elements = tree.iter_elements();

    let tables: Vec<&StructElem> = elements
        .iter()
        .filter(|e| e.role == StandardRole::Table)
        .copied()
        .collect();

    if tables.is_empty() {
        return AccessibilityCheck {
            id: "table-headers",
            description: "Tables must contain header cells (TH)",
            passed: true,
            details: vec![],
        };
    }

    let mut details = Vec::new();
    for (i, table) in tables.iter().enumerate() {
        let has_th = has_descendant_role(table, &StandardRole::TH);
        if !has_th {
            details.push(format!("Table #{} has no TH (header) cells", i + 1));
        }
    }

    AccessibilityCheck {
        id: "table-headers",
        description: "Tables must contain header cells (TH)",
        passed: details.is_empty(),
        details,
    }
}

/// Check 6 (PDF/UA-2): Note elements should use FENote instead.
///
/// PDF/UA-2 (ISO 14289-2:2024) replaces the `Note` structure element
/// with `FENote` (footnote/endnote). Documents using `Note` are flagged
/// as not conforming to PDF/UA-2.
fn check_pdfua2_fenote(tree: &StructTree) -> AccessibilityCheck {
    let elements = tree.iter_elements();
    let deprecated_notes: Vec<String> = elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.role == StandardRole::Note)
        .map(|(i, _)| {
            format!(
                "Element #{} uses deprecated Note; use FENote for PDF/UA-2",
                i + 1
            )
        })
        .collect();

    AccessibilityCheck {
        id: "pdfua2-fenote",
        description: "PDF/UA-2: Use FENote instead of deprecated Note element",
        passed: deprecated_notes.is_empty(),
        details: deprecated_notes,
    }
}

/// Checks whether an element or any of its descendants has the given role.
fn has_descendant_role(elem: &StructElem, role: &StandardRole) -> bool {
    if &elem.role == role {
        return true;
    }
    elem.children.iter().any(|c| has_descendant_role(c, role))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::structure_tree::{RoleMap, StructTree};

    fn make_elem(struct_type: &str, children: Vec<StructElem>) -> StructElem {
        StructElem {
            struct_type: struct_type.into(),
            role: StandardRole::from_name(struct_type),
            title: None,
            lang: None,
            alt_text: None,
            actual_text: None,
            children,
        }
    }

    fn make_figure(alt: Option<&str>) -> StructElem {
        StructElem {
            struct_type: "Figure".into(),
            role: StandardRole::Figure,
            title: None,
            lang: None,
            alt_text: alt.map(|s| s.into()),
            actual_text: None,
            children: vec![],
        }
    }

    #[test]
    fn empty_tree_fails_tagged_check() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![],
            lang: None,
        };
        let report = validate_pdf_ua(&tree);
        assert!(!report.is_compliant());

        let tagged = report.checks.iter().find(|c| c.id == "tagged-pdf").unwrap();
        assert!(!tagged.passed);
    }

    #[test]
    fn missing_language_fails() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem("Document", vec![])],
            lang: None,
        };
        let report = validate_pdf_ua(&tree);
        let lang = report
            .checks
            .iter()
            .find(|c| c.id == "document-language")
            .unwrap();
        assert!(!lang.passed);
    }

    #[test]
    fn present_language_passes() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem("Document", vec![])],
            lang: Some("en-US".into()),
        };
        let report = validate_pdf_ua(&tree);
        let lang = report
            .checks
            .iter()
            .find(|c| c.id == "document-language")
            .unwrap();
        assert!(lang.passed);
    }

    #[test]
    fn figures_with_alt_text_pass() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem("Document", vec![make_figure(Some("A photo"))])],
            lang: Some("en".into()),
        };
        let report = validate_pdf_ua(&tree);
        let fig = report
            .checks
            .iter()
            .find(|c| c.id == "figure-alt-text")
            .unwrap();
        assert!(fig.passed);
    }

    #[test]
    fn figures_without_alt_text_fail() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem(
                "Document",
                vec![make_figure(None), make_figure(Some("OK"))],
            )],
            lang: Some("en".into()),
        };
        let report = validate_pdf_ua(&tree);
        let fig = report
            .checks
            .iter()
            .find(|c| c.id == "figure-alt-text")
            .unwrap();
        assert!(!fig.passed);
        assert_eq!(fig.details.len(), 1);
    }

    #[test]
    fn proper_heading_order_passes() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem(
                "Document",
                vec![make_elem("H1", vec![]), make_elem("H2", vec![])],
            )],
            lang: Some("en".into()),
        };
        let report = validate_pdf_ua(&tree);
        let heading = report
            .checks
            .iter()
            .find(|c| c.id == "heading-order")
            .unwrap();
        assert!(heading.passed);
    }

    #[test]
    fn skipped_heading_level_fails() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem(
                "Document",
                vec![make_elem("H1", vec![]), make_elem("H3", vec![])],
            )],
            lang: Some("en".into()),
        };
        let report = validate_pdf_ua(&tree);
        let heading = report
            .checks
            .iter()
            .find(|c| c.id == "heading-order")
            .unwrap();
        assert!(!heading.passed);
        assert!(heading.details[0].contains("skips level 2"));
    }

    #[test]
    fn table_with_headers_passes() {
        let table = StructElem {
            struct_type: "Table".into(),
            role: StandardRole::Table,
            title: None,
            lang: None,
            alt_text: None,
            actual_text: None,
            children: vec![StructElem {
                struct_type: "TR".into(),
                role: StandardRole::TR,
                title: None,
                lang: None,
                alt_text: None,
                actual_text: None,
                children: vec![make_elem("TH", vec![]), make_elem("TD", vec![])],
            }],
        };

        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem("Document", vec![table])],
            lang: Some("en".into()),
        };

        let report = validate_pdf_ua(&tree);
        let th = report
            .checks
            .iter()
            .find(|c| c.id == "table-headers")
            .unwrap();
        assert!(th.passed);
    }

    #[test]
    fn table_without_headers_fails() {
        let table = StructElem {
            struct_type: "Table".into(),
            role: StandardRole::Table,
            title: None,
            lang: None,
            alt_text: None,
            actual_text: None,
            children: vec![StructElem {
                struct_type: "TR".into(),
                role: StandardRole::TR,
                title: None,
                lang: None,
                alt_text: None,
                actual_text: None,
                children: vec![make_elem("TD", vec![]), make_elem("TD", vec![])],
            }],
        };

        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem("Document", vec![table])],
            lang: Some("en".into()),
        };

        let report = validate_pdf_ua(&tree);
        let th = report
            .checks
            .iter()
            .find(|c| c.id == "table-headers")
            .unwrap();
        assert!(!th.passed);
    }

    #[test]
    fn fully_compliant_document() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem(
                "Document",
                vec![
                    make_elem("H1", vec![]),
                    make_elem("P", vec![]),
                    make_figure(Some("Photo of sunset")),
                ],
            )],
            lang: Some("en-US".into()),
        };

        let report = validate_pdf_ua(&tree);
        assert!(report.is_compliant());
        assert_eq!(report.passed_count(), 6);
        assert_eq!(report.total_checks(), 6);
        assert!(report.failures().is_empty());
    }

    #[test]
    fn no_tables_passes_table_check() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem("Document", vec![make_elem("P", vec![])])],
            lang: Some("en".into()),
        };
        let report = validate_pdf_ua(&tree);
        let th = report
            .checks
            .iter()
            .find(|c| c.id == "table-headers")
            .unwrap();
        assert!(th.passed);
    }

    // --- PDF/UA-2 validation checks ---

    #[test]
    fn pdfua2_note_deprecated_warning() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem(
                "Document",
                vec![make_elem("P", vec![]), make_elem("Note", vec![])],
            )],
            lang: Some("en".into()),
        };
        let report = validate_pdf_ua(&tree);
        let note_check = report
            .checks
            .iter()
            .find(|c| c.id == "pdfua2-fenote")
            .unwrap();
        assert!(!note_check.passed);
        assert!(note_check.details[0].contains("FENote"));
    }

    #[test]
    fn pdfua2_fenote_passes() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem(
                "Document",
                vec![make_elem("P", vec![]), make_elem("FENote", vec![])],
            )],
            lang: Some("en".into()),
        };
        let report = validate_pdf_ua(&tree);
        let note_check = report
            .checks
            .iter()
            .find(|c| c.id == "pdfua2-fenote")
            .unwrap();
        assert!(note_check.passed);
    }

    #[test]
    fn pdfua2_em_and_strong_in_compliant_doc() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![make_elem(
                "Document",
                vec![make_elem(
                    "P",
                    vec![make_elem("Em", vec![]), make_elem("Strong", vec![])],
                )],
            )],
            lang: Some("en".into()),
        };
        let report = validate_pdf_ua(&tree);
        // Em and Strong are valid inline elements — no new checks should fail
        let failures: Vec<_> = report.failures();
        assert!(
            failures.is_empty(),
            "Em and Strong should not cause failures: {:?}",
            failures
        );
    }

    #[test]
    fn report_failures_helper() {
        let tree = StructTree {
            role_map: RoleMap::new(),
            children: vec![],
            lang: None,
        };
        let report = validate_pdf_ua(&tree);
        let failures = report.failures();
        assert!(failures.len() >= 2); // at least tagged-pdf and document-language
    }
}
