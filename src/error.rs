//! Error types for PDFPurr
//!
//! This module defines the error types used throughout the library.

use std::fmt;
use std::io;

/// Main error type for PDFPurr operations
#[derive(Debug)]
pub enum PdfError {
    /// I/O error occurred
    Io(io::Error),

    /// PDF syntax error
    SyntaxError {
        /// Position in the file where the error occurred
        position: usize,
        /// Description of the syntax error
        message: String,
    },

    /// Invalid PDF structure
    InvalidStructure(String),

    /// Encryption-related error
    EncryptionError(String),

    /// Password required to open the document
    PasswordRequired,

    /// Invalid password provided
    InvalidPassword,

    /// Document does not comply with a standard
    NonCompliant {
        /// The standard (e.g., "PDF/A-1", "PDF/UA")
        standard: String,
        /// Reason for non-compliance
        reason: String,
    },

    /// Feature not yet supported
    UnsupportedFeature(String),

    /// Type error (expected one type, found another)
    TypeError {
        /// Expected type
        expected: String,
        /// Found type
        found: String,
    },

    /// Resource not found
    ResourceNotFound(String),

    /// Invalid resource
    InvalidResource(String),

    /// Parsing error
    ParseError(String),

    /// Invalid object reference
    InvalidReference(String),

    /// Invalid font
    InvalidFont(String),

    /// Invalid image
    InvalidImage(String),

    /// Invalid annotation
    InvalidAnnotation(String),

    /// Invalid form field
    InvalidFormField(String),

    /// Invalid page
    InvalidPage(String),

    /// Cross-reference table error
    XRefError(String),

    /// Compression/decompression error
    CompressionError(String),

    /// Encoding error
    EncodingError(String),

    /// Generic error with a message
    Other(String),
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfError::Io(err) => write!(f, "I/O error: {}", err),
            PdfError::SyntaxError { position, message } => {
                write!(f, "Syntax error at position {}: {}", position, message)
            }
            PdfError::InvalidStructure(msg) => write!(f, "Invalid PDF structure: {}", msg),
            PdfError::EncryptionError(msg) => write!(f, "Encryption error: {}", msg),
            PdfError::PasswordRequired => write!(f, "Password required to open this document"),
            PdfError::InvalidPassword => write!(f, "Invalid password provided"),
            PdfError::NonCompliant { standard, reason } => {
                write!(f, "Not compliant with {}: {}", standard, reason)
            }
            PdfError::UnsupportedFeature(msg) => write!(f, "Unsupported feature: {}", msg),
            PdfError::TypeError { expected, found } => {
                write!(f, "Type error: expected {}, found {}", expected, found)
            }
            PdfError::ResourceNotFound(msg) => write!(f, "Resource not found: {}", msg),
            PdfError::InvalidResource(msg) => write!(f, "Invalid resource: {}", msg),
            PdfError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            PdfError::InvalidReference(msg) => write!(f, "Invalid reference: {}", msg),
            PdfError::InvalidFont(msg) => write!(f, "Invalid font: {}", msg),
            PdfError::InvalidImage(msg) => write!(f, "Invalid image: {}", msg),
            PdfError::InvalidAnnotation(msg) => write!(f, "Invalid annotation: {}", msg),
            PdfError::InvalidFormField(msg) => write!(f, "Invalid form field: {}", msg),
            PdfError::InvalidPage(msg) => write!(f, "Invalid page: {}", msg),
            PdfError::XRefError(msg) => write!(f, "Cross-reference error: {}", msg),
            PdfError::CompressionError(msg) => write!(f, "Compression error: {}", msg),
            PdfError::EncodingError(msg) => write!(f, "Encoding error: {}", msg),
            PdfError::Other(msg) => write!(f, "Error: {}", msg),
        }
    }
}

impl std::error::Error for PdfError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PdfError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for PdfError {
    fn from(err: io::Error) -> Self {
        PdfError::Io(err)
    }
}

impl From<std::string::FromUtf8Error> for PdfError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        PdfError::EncodingError(format!("UTF-8 conversion error: {}", err))
    }
}

/// Result type for PDFPurr operations
pub type PdfResult<T> = Result<T, PdfError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = PdfError::SyntaxError {
            position: 42,
            message: "Unexpected token".to_string(),
        };
        assert_eq!(
            format!("{}", err),
            "Syntax error at position 42: Unexpected token"
        );
    }

    #[test]
    fn test_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let pdf_err: PdfError = io_err.into();
        assert!(matches!(pdf_err, PdfError::Io(_)));
    }

    #[test]
    fn test_type_error() {
        let err = PdfError::TypeError {
            expected: "Dictionary".to_string(),
            found: "Array".to_string(),
        };
        assert_eq!(
            format!("{}", err),
            "Type error: expected Dictionary, found Array"
        );
    }
}
