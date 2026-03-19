//! Shared types for PDF standards compliance validation.
//!
//! Provides [`StandardsCheck`] and [`StandardsReport`] (mirroring the
//! accessibility validation pattern), plus conformance level enums for
//! PDF/A (ISO 19005) and PDF/X (ISO 15930).

use std::fmt;

// --- Check ID constants ---

/// Check: document must not be encrypted.
pub const CHECK_NO_ENCRYPTION: &str = "no-encryption";
/// Check: all fonts must be embedded.
pub const CHECK_FONTS_EMBEDDED: &str = "fonts-embedded";
/// Check: XMP metadata with PDF/A identification required.
pub const CHECK_XMP_METADATA: &str = "xmp-metadata";
/// Check: LZWDecode filter is not permitted.
pub const CHECK_NO_LZW: &str = "no-lzw-filters";
/// Check: JavaScript is not permitted.
pub const CHECK_NO_JAVASCRIPT: &str = "no-javascript";
/// Check: Info dict and XMP metadata must be consistent.
pub const CHECK_METADATA_CONSISTENCY: &str = "metadata-consistency";
/// Check: device-dependent color spaces require OutputIntents.
pub const CHECK_COLOR_SPACES: &str = "color-spaces";
/// Check: no transparency groups allowed.
pub const CHECK_NO_TRANSPARENCY: &str = "no-transparency";
/// Check: OutputIntents with GTS_PDFX must be present.
pub const CHECK_OUTPUT_INTENT_PDFX: &str = "output-intent-pdfx";
/// Check: every page must have TrimBox or ArtBox.
pub const CHECK_TRIM_OR_ART_BOX: &str = "trim-or-art-box";

/// An individual standards compliance check result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardsCheck {
    /// Short identifier for the check (e.g. `"no-encryption"`).
    pub id: &'static str,
    /// Human-readable description of the requirement.
    pub description: &'static str,
    /// Whether the check passed.
    pub passed: bool,
    /// Details about failures (empty if passed).
    pub details: Vec<String>,
}

impl StandardsCheck {
    /// Creates a passing check with no details.
    pub fn pass(id: &'static str, description: &'static str) -> Self {
        Self {
            id,
            description,
            passed: true,
            details: vec![],
        }
    }

    /// Creates a failing check with a single detail message.
    pub fn fail(id: &'static str, description: &'static str, detail: impl Into<String>) -> Self {
        Self {
            id,
            description,
            passed: false,
            details: vec![detail.into()],
        }
    }

    /// Creates a check from a details list — passes if details is empty.
    pub fn from_details(id: &'static str, description: &'static str, details: Vec<String>) -> Self {
        Self {
            id,
            description,
            passed: details.is_empty(),
            details,
        }
    }
}

/// A standards validation report containing individual check results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardsReport {
    /// The standard being validated (e.g. `"PDF/A-1b"`).
    pub standard: String,
    /// The individual check results.
    pub checks: Vec<StandardsCheck>,
}

impl StandardsReport {
    /// Returns `true` if all checks passed.
    pub fn is_compliant(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    /// Returns only the failed checks.
    pub fn failures(&self) -> Vec<&StandardsCheck> {
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

/// PDF/A conformance level (ISO 19005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfALevel {
    /// PDF/A-1b — basic conformance, based on PDF 1.4.
    A1b,
    /// PDF/A-2b — basic conformance, based on PDF 1.7.
    A2b,
    /// PDF/A-3b — basic conformance with embedded files, based on PDF 1.7.
    A3b,
}

impl fmt::Display for PdfALevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfALevel::A1b => write!(f, "PDF/A-1b"),
            PdfALevel::A2b => write!(f, "PDF/A-2b"),
            PdfALevel::A3b => write!(f, "PDF/A-3b"),
        }
    }
}

/// PDF/X conformance level (ISO 15930).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfXLevel {
    /// PDF/X-1a:2001 — CMYK and spot colors only, no transparency.
    X1a,
    /// PDF/X-3:2002 — allows color-managed RGB.
    X3,
    /// PDF/X-4 — allows transparency and layers.
    X4,
}

impl fmt::Display for PdfXLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfXLevel::X1a => write!(f, "PDF/X-1a:2001"),
            PdfXLevel::X3 => write!(f, "PDF/X-3:2002"),
            PdfXLevel::X4 => write!(f, "PDF/X-4"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standards_check_creation() {
        let check = StandardsCheck {
            id: "test-check",
            description: "A test check",
            passed: true,
            details: vec![],
        };
        assert!(check.passed);
        assert_eq!(check.id, "test-check");
        assert!(check.details.is_empty());
    }

    #[test]
    fn standards_check_failing() {
        let check = StandardsCheck {
            id: "fail-check",
            description: "A failing check",
            passed: false,
            details: vec!["Missing required field".into()],
        };
        assert!(!check.passed);
        assert_eq!(check.details.len(), 1);
    }

    #[test]
    fn standards_report_all_pass() {
        let report = StandardsReport {
            standard: "PDF/A-1b".into(),
            checks: vec![
                StandardsCheck {
                    id: "c1",
                    description: "Check 1",
                    passed: true,
                    details: vec![],
                },
                StandardsCheck {
                    id: "c2",
                    description: "Check 2",
                    passed: true,
                    details: vec![],
                },
            ],
        };
        assert!(report.is_compliant());
        assert!(report.failures().is_empty());
    }

    #[test]
    fn standards_report_with_failures() {
        let report = StandardsReport {
            standard: "PDF/A-1b".into(),
            checks: vec![
                StandardsCheck {
                    id: "pass",
                    description: "Passes",
                    passed: true,
                    details: vec![],
                },
                StandardsCheck {
                    id: "fail",
                    description: "Fails",
                    passed: false,
                    details: vec!["bad".into()],
                },
            ],
        };
        assert!(!report.is_compliant());
        let failures = report.failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].id, "fail");
    }

    #[test]
    fn standards_report_counts() {
        let report = StandardsReport {
            standard: "PDF/X-4".into(),
            checks: vec![
                StandardsCheck {
                    id: "a",
                    description: "A",
                    passed: true,
                    details: vec![],
                },
                StandardsCheck {
                    id: "b",
                    description: "B",
                    passed: false,
                    details: vec!["err".into()],
                },
                StandardsCheck {
                    id: "c",
                    description: "C",
                    passed: true,
                    details: vec![],
                },
            ],
        };
        assert_eq!(report.total_checks(), 3);
        assert_eq!(report.passed_count(), 2);
    }

    #[test]
    fn pdfa_level_display() {
        assert_eq!(format!("{}", PdfALevel::A1b), "PDF/A-1b");
        assert_eq!(format!("{}", PdfALevel::A2b), "PDF/A-2b");
        assert_eq!(format!("{}", PdfALevel::A3b), "PDF/A-3b");
    }

    #[test]
    fn pdfx_level_display() {
        assert_eq!(format!("{}", PdfXLevel::X1a), "PDF/X-1a:2001");
        assert_eq!(format!("{}", PdfXLevel::X3), "PDF/X-3:2002");
        assert_eq!(format!("{}", PdfXLevel::X4), "PDF/X-4");
    }

    #[test]
    fn standards_report_empty() {
        let report = StandardsReport {
            standard: "Test".into(),
            checks: vec![],
        };
        assert!(report.is_compliant());
        assert_eq!(report.total_checks(), 0);
        assert_eq!(report.passed_count(), 0);
    }
}
