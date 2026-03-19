//! PDF function evaluation (ISO 32000-2:2020, Section 7.10).
//!
//! Provides a reusable evaluator for PDF functions used in shading patterns,
//! tint transforms (Separation/DeviceN), and other contexts. Supports
//! Type 2 (exponential interpolation) and Type 3 (stitching) functions.

use crate::core::objects::{DictExt, Dictionary, Object};
use crate::document::Document;

use super::colors::{obj_f32, parse_f32_array};

/// A PDF function that maps input values to output values.
#[derive(Debug, Clone)]
pub(crate) enum PdfFunction {
    /// Type 0: Sampled function (lookup table with interpolation).
    Sampled {
        /// Domain [min, max] for the single input dimension.
        domain: [f32; 2],
        /// Range pairs for each output dimension.
        range: Vec<f32>,
        /// Number of samples along the input dimension.
        size: u32,
        /// Encode mapping: [e0, e1] from domain to sample index range.
        encode: [f32; 2],
        /// Decode pairs for each output dimension.
        decode: Vec<f32>,
        /// Number of output values per sample.
        n_outputs: usize,
        /// Pre-decoded sample values (flattened: n_outputs per grid point).
        samples: Vec<f32>,
    },
    /// Type 2: Exponential interpolation.
    ///
    /// `output_i = C0_i + x^N * (C1_i - C0_i)`
    Exponential {
        /// Output values at input 0 (default [0.0]).
        c0: Vec<f32>,
        /// Output values at input 1 (default [1.0]).
        c1: Vec<f32>,
        /// Interpolation exponent (default 1.0).
        n: f32,
    },
    /// Type 3: Stitching function.
    ///
    /// Combines multiple sub-functions over contiguous subdomains.
    Stitching {
        /// Sub-functions, one per subdomain.
        functions: Vec<PdfFunction>,
        /// Boundary values between subdomains (k-1 values for k functions).
        bounds: Vec<f32>,
        /// Encode array: pairs [e0_0, e1_0, e0_1, e1_1, ...] mapping each
        /// subdomain to the sub-function's domain.
        encode: Vec<f32>,
        /// Overall domain [min, max].
        domain: [f32; 2],
    },
}

impl PdfFunction {
    /// Evaluates the function for a single input value, writing results into `out`.
    ///
    /// Clears `out` and fills it with the output components.
    pub fn evaluate_into(&self, input: f32, out: &mut Vec<f32>) {
        out.clear();
        match self {
            Self::Sampled {
                domain,
                range,
                size,
                encode,
                decode,
                n_outputs,
                samples,
            } => {
                let x = input.clamp(domain[0], domain[1]);

                // Map input from domain to encode range
                let d_range = domain[1] - domain[0];
                let norm = if d_range.abs() < f32::EPSILON {
                    0.0
                } else {
                    (x - domain[0]) / d_range
                };
                let encoded = encode[0] + norm * (encode[1] - encode[0]);

                // Clamp to valid sample index range [0, size-1]
                let max_idx = (*size as f32) - 1.0;
                let clamped = encoded.clamp(0.0, max_idx);

                // Linear interpolation between bracketing samples
                let i0 = clamped.floor() as usize;
                let i1 = (i0 + 1).min(*size as usize - 1);
                let frac = clamped - clamped.floor();

                for j in 0..*n_outputs {
                    let s0 = samples.get(i0 * n_outputs + j).copied().unwrap_or(0.0);
                    let s1 = samples.get(i1 * n_outputs + j).copied().unwrap_or(0.0);
                    let interp = s0 + frac * (s1 - s0);

                    // Map through decode
                    let d0 = decode.get(j * 2).copied().unwrap_or(0.0);
                    let d1 = decode.get(j * 2 + 1).copied().unwrap_or(1.0);
                    let val = d0 + interp * (d1 - d0);

                    // Clamp to range if present
                    let r0 = range.get(j * 2).copied().unwrap_or(f32::NEG_INFINITY);
                    let r1 = range.get(j * 2 + 1).copied().unwrap_or(f32::INFINITY);
                    out.push(val.clamp(r0, r1));
                }
            }
            Self::Exponential { c0, c1, n } => {
                let x = input.clamp(0.0, 1.0);
                let t = if *n == 1.0 { x } else { x.powf(*n) };
                out.extend(c0.iter().zip(c1.iter()).map(|(&a, &b)| a + t * (b - a)));
            }
            Self::Stitching {
                functions,
                bounds,
                encode,
                domain,
            } => {
                let x = input.clamp(domain[0], domain[1]);

                // Find subdomain via partition_point (binary search).
                // Per PDF spec, boundary values are inclusive on the left for each
                // subdomain except the first; partition_point gives the first bound > x.
                let idx = bounds.partition_point(|&b| b <= x).min(functions.len() - 1);

                // Subdomain bounds
                let sub_start = if idx == 0 { domain[0] } else { bounds[idx - 1] };
                let sub_end = if idx < bounds.len() {
                    bounds[idx]
                } else {
                    domain[1]
                };

                // Map x from [sub_start, sub_end] to encode range [e0, e1]
                let e0 = encode.get(idx * 2).copied().unwrap_or(0.0);
                let e1 = encode.get(idx * 2 + 1).copied().unwrap_or(1.0);

                let range = sub_end - sub_start;
                let mapped = if range.abs() < f32::EPSILON {
                    e0
                } else {
                    e0 + (x - sub_start) / range * (e1 - e0)
                };

                functions[idx].evaluate_into(mapped, out);
            }
        }
    }

    /// Convenience wrapper that allocates and returns a new `Vec<f32>`.
    pub fn evaluate(&self, input: f32) -> Vec<f32> {
        let mut out = Vec::new();
        self.evaluate_into(input, &mut out);
        out
    }

    /// Evaluates the function with multiple input values (for DeviceN tint transforms).
    ///
    /// For single-input function types (Type 2, Type 3, 1-D Type 0), uses the first
    /// input value. Multi-dimensional Type 0 sampled functions use all inputs.
    pub fn evaluate_multi(&self, inputs: &[f32]) -> Vec<f32> {
        // All currently supported function types (Type 0/2/3) are single-input.
        // Use the first input component; multi-dimensional Type 0 would need
        // additional Size entries and n-D interpolation.
        let input = inputs.first().copied().unwrap_or(0.0);
        self.evaluate(input)
    }

    /// Creates an identity-like linear function mapping `[0, 1]` to `[0…0, 1…1]`.
    ///
    /// Used as a fallback when a tint transform is missing from the PDF.
    pub fn identity(num_components: usize) -> Self {
        Self::Exponential {
            c0: vec![0.0; num_components],
            c1: vec![1.0; num_components],
            n: 1.0,
        }
    }

    /// Parses a PDF function from a dictionary object.
    pub fn from_object(obj: &Object, doc: &Document) -> Option<Self> {
        // Type 0 requires stream data, so handle it before falling through to dict
        if let Object::Stream(s) = obj {
            let func_type = match s.dict.get_str("FunctionType") {
                Some(Object::Integer(t)) => *t,
                _ => return None,
            };
            if func_type == 0 {
                return Self::parse_type0(s);
            }
            return Self::from_dict(&s.dict, doc);
        }
        let dict = match obj {
            Object::Dictionary(d) => d,
            _ => return None,
        };
        Self::from_dict(dict, doc)
    }

    /// Parses from a PDF dictionary.
    pub fn from_dict(dict: &Dictionary, doc: &Document) -> Option<Self> {
        let func_type = match dict.get_str("FunctionType") {
            Some(Object::Integer(t)) => *t,
            _ => return None,
        };

        match func_type {
            2 => Self::parse_type2(dict),
            3 => Self::parse_type3(dict, doc),
            _ => None,
        }
    }

    /// Parses a Type 0 sampled function from a stream.
    fn parse_type0(stream: &crate::core::objects::PdfStream) -> Option<Self> {
        let dict = &stream.dict;

        let size_arr = dict.get_str("Size")?.as_array()?;
        if size_arr.is_empty() {
            return None;
        }
        // Only support 1-D sampled functions for now
        let size = match &size_arr[0] {
            Object::Integer(n) => *n as u32,
            _ => return None,
        };
        if size == 0 {
            return None;
        }

        let bits_per_sample = match dict.get_str("BitsPerSample") {
            Some(Object::Integer(n)) => *n as u32,
            _ => return None,
        };

        let domain = parse_domain(dict.get_str("Domain"));
        let range = parse_f32_array(dict.get_str("Range")).unwrap_or_default();

        // Determine number of output dimensions from Range (pairs of min/max)
        let n_outputs = if range.len() >= 2 { range.len() / 2 } else { 1 };

        // Encode: default [0, Size-1]
        let encode_arr = parse_f32_array(dict.get_str("Encode"));
        let encode = match encode_arr {
            Some(ref e) if e.len() >= 2 => [e[0], e[1]],
            _ => [0.0, (size - 1) as f32],
        };

        // Decode: default = Range
        let decode = parse_f32_array(dict.get_str("Decode")).unwrap_or_else(|| range.clone());

        // Extract sample data
        let data = stream.decode_data().ok()?;
        let total_samples = size as usize * n_outputs;
        let samples = extract_samples(&data, bits_per_sample, total_samples);

        Some(Self::Sampled {
            domain,
            range,
            size,
            encode,
            decode,
            n_outputs,
            samples,
        })
    }

    /// Parses a Type 2 exponential interpolation function.
    fn parse_type2(dict: &Dictionary) -> Option<Self> {
        let c0 = parse_f32_array(dict.get_str("C0")).unwrap_or_else(|| vec![0.0]);
        let c1 = parse_f32_array(dict.get_str("C1")).unwrap_or_else(|| vec![1.0]);
        let n = dict.get_str("N").and_then(obj_f32).unwrap_or(1.0);
        Some(Self::Exponential { c0, c1, n })
    }

    /// Parses a Type 3 stitching function.
    fn parse_type3(dict: &Dictionary, doc: &Document) -> Option<Self> {
        let func_arr = dict.get_str("Functions")?.as_array()?;
        let mut functions = Vec::with_capacity(func_arr.len());
        for obj in func_arr {
            let resolved = doc.resolve(obj).unwrap_or(obj);
            functions.push(Self::from_object(resolved, doc)?);
        }

        let bounds = parse_f32_array(dict.get_str("Bounds")).unwrap_or_default();
        let encode = parse_f32_array(dict.get_str("Encode")).unwrap_or_default();
        let domain = parse_domain(dict.get_str("Domain"));

        Some(Self::Stitching {
            functions,
            bounds,
            encode,
            domain,
        })
    }
}

/// Extracts sample values from a byte stream, normalizing to [0.0, 1.0].
fn extract_samples(data: &[u8], bits_per_sample: u32, count: usize) -> Vec<f32> {
    let max_val = ((1u64 << bits_per_sample) - 1) as f32;
    if max_val == 0.0 {
        return vec![0.0; count];
    }

    let mut samples = Vec::with_capacity(count);
    match bits_per_sample {
        8 => {
            for i in 0..count {
                let val = data.get(i).copied().unwrap_or(0) as f32;
                samples.push(val / max_val);
            }
        }
        16 => {
            for i in 0..count {
                let hi = data.get(i * 2).copied().unwrap_or(0) as u16;
                let lo = data.get(i * 2 + 1).copied().unwrap_or(0) as u16;
                let val = ((hi << 8) | lo) as f32;
                samples.push(val / max_val);
            }
        }
        _ => {
            // Bit-packed extraction for other sizes (1, 2, 4, 12, 24, 32)
            let mut bit_offset = 0usize;
            for _ in 0..count {
                let byte_idx = bit_offset / 8;
                let bit_idx = bit_offset % 8;
                let mut val: u32 = 0;
                let mut bits_remaining = bits_per_sample;
                let mut cur_byte = byte_idx;
                let mut cur_bit = bit_idx;

                while bits_remaining > 0 {
                    let available = 8 - cur_bit;
                    let take = bits_remaining.min(available as u32);
                    let byte_val = data.get(cur_byte).copied().unwrap_or(0);
                    let shift = available as u32 - take;
                    let mask = ((1u32 << take) - 1) << shift;
                    let extracted = ((byte_val as u32) & mask) >> shift;
                    val = (val << take) | extracted;
                    bits_remaining -= take;
                    cur_byte += 1;
                    cur_bit = 0;
                }

                samples.push(val as f32 / max_val);
                bit_offset += bits_per_sample as usize;
            }
        }
    }
    samples
}

/// Parses a [min, max] domain from a PDF object.
fn parse_domain(obj: Option<&Object>) -> [f32; 2] {
    match obj.and_then(|o| o.as_array()) {
        Some(arr) if arr.len() >= 2 => {
            let d0 = obj_f32(&arr[0]).unwrap_or(0.0);
            let d1 = obj_f32(&arr[1]).unwrap_or(1.0);
            [d0, d1]
        }
        _ => [0.0, 1.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::PdfName;

    /// Helper: number of output components for a function.
    fn output_size(f: &PdfFunction) -> usize {
        match f {
            PdfFunction::Sampled { n_outputs, .. } => *n_outputs,
            PdfFunction::Exponential { c0, c1, .. } => c0.len().max(c1.len()),
            PdfFunction::Stitching { functions, .. } => {
                functions.first().map_or(0, |f| output_size(f))
            }
        }
    }

    /// Helper: creates a single-segment identity stitching function.
    fn single_identity_stitch() -> PdfFunction {
        PdfFunction::Stitching {
            functions: vec![PdfFunction::Exponential {
                c0: vec![0.0],
                c1: vec![1.0],
                n: 1.0,
            }],
            bounds: vec![],
            encode: vec![0.0, 1.0],
            domain: [0.0, 1.0],
        }
    }

    // ---- Type 2 (Exponential) tests ----

    #[test]
    fn type2_linear_interpolation() {
        let f = PdfFunction::Exponential {
            c0: vec![0.0, 0.0, 0.0],
            c1: vec![1.0, 0.5, 0.25],
            n: 1.0,
        };
        let result = f.evaluate(0.5);
        assert_eq!(result.len(), 3);
        assert!((result[0] - 0.5).abs() < 1e-5);
        assert!((result[1] - 0.25).abs() < 1e-5);
        assert!((result[2] - 0.125).abs() < 1e-5);
    }

    #[test]
    fn type2_at_endpoints() {
        let f = PdfFunction::Exponential {
            c0: vec![0.2],
            c1: vec![0.8],
            n: 1.0,
        };
        assert!((f.evaluate(0.0)[0] - 0.2).abs() < 1e-5);
        assert!((f.evaluate(1.0)[0] - 0.8).abs() < 1e-5);
    }

    #[test]
    fn type2_quadratic_exponent() {
        let f = PdfFunction::Exponential {
            c0: vec![0.0],
            c1: vec![1.0],
            n: 2.0,
        };
        assert!((f.evaluate(0.5)[0] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn type2_clamps_input() {
        let f = PdfFunction::Exponential {
            c0: vec![0.0],
            c1: vec![1.0],
            n: 1.0,
        };
        assert!((f.evaluate(-1.0)[0]).abs() < 1e-5);
        assert!((f.evaluate(2.0)[0] - 1.0).abs() < 1e-5);
    }

    // ---- Type 3 (Stitching) tests ----

    #[test]
    fn type3_two_segment_stitching() {
        let f = PdfFunction::Stitching {
            functions: vec![
                PdfFunction::Exponential {
                    c0: vec![0.0],
                    c1: vec![1.0],
                    n: 1.0,
                },
                PdfFunction::Exponential {
                    c0: vec![1.0],
                    c1: vec![0.0],
                    n: 1.0,
                },
            ],
            bounds: vec![0.5],
            encode: vec![0.0, 1.0, 0.0, 1.0],
            domain: [0.0, 1.0],
        };

        assert!((f.evaluate(0.0)[0]).abs() < 1e-5);
        assert!((f.evaluate(0.25)[0] - 0.5).abs() < 1e-5);
        assert!((f.evaluate(0.5)[0] - 1.0).abs() < 1e-5);
        assert!((f.evaluate(1.0)[0]).abs() < 1e-5);
    }

    #[test]
    fn type3_single_function_and_clamping() {
        let f = single_identity_stitch();
        // Normal evaluation
        assert!((f.evaluate(0.5)[0] - 0.5).abs() < 1e-5);
        // Clamping: input -1.0 → 0.0, input 2.0 → 1.0
        assert!((f.evaluate(-1.0)[0]).abs() < 1e-5);
        assert!((f.evaluate(2.0)[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn type3_evaluate_into_reuses_buffer() {
        let f = single_identity_stitch();
        let mut buf = Vec::new();
        f.evaluate_into(0.5, &mut buf);
        assert!((buf[0] - 0.5).abs() < 1e-5);
        // Re-evaluate into same buffer — no new allocation
        f.evaluate_into(0.75, &mut buf);
        assert!((buf[0] - 0.75).abs() < 1e-5);
        assert_eq!(buf.len(), 1);
    }

    // ---- output_size tests ----

    #[test]
    fn output_size_exponential() {
        let f = PdfFunction::Exponential {
            c0: vec![0.0, 0.0, 0.0],
            c1: vec![1.0, 1.0, 1.0],
            n: 1.0,
        };
        assert_eq!(output_size(&f), 3);
    }

    #[test]
    fn output_size_stitching() {
        let f = single_identity_stitch();
        assert_eq!(output_size(&f), 1);
    }

    // ---- Identity factory tests ----

    #[test]
    fn identity_function() {
        let f = PdfFunction::identity(3);
        let result = f.evaluate(0.5);
        assert_eq!(result.len(), 3);
        assert!((result[0] - 0.5).abs() < 1e-5);
        assert!((result[1] - 0.5).abs() < 1e-5);
        assert!((result[2] - 0.5).abs() < 1e-5);
    }

    // ---- evaluate_multi tests ----

    #[test]
    fn evaluate_multi_uses_first_input_for_type2() {
        let f = PdfFunction::Exponential {
            c0: vec![0.0, 0.0],
            c1: vec![1.0, 0.5],
            n: 1.0,
        };
        let result = f.evaluate_multi(&[0.5, 0.8]);
        assert_eq!(result.len(), 2);
        // Should use first input (0.5), not second (0.8)
        assert!((result[0] - 0.5).abs() < 1e-5);
        assert!((result[1] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn evaluate_multi_empty_input() {
        let f = PdfFunction::Exponential {
            c0: vec![0.0],
            c1: vec![1.0],
            n: 1.0,
        };
        let result = f.evaluate_multi(&[]);
        assert!((result[0]).abs() < 1e-5); // defaults to 0.0
    }

    // ---- Parsing tests ----

    #[test]
    fn parse_type2_from_dict() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("FunctionType"), Object::Integer(2));
        dict.insert(PdfName::new("N"), Object::Real(1.0));
        dict.insert(
            PdfName::new("C0"),
            Object::Array(vec![Object::Real(0.0), Object::Real(0.0)]),
        );
        dict.insert(
            PdfName::new("C1"),
            Object::Array(vec![Object::Real(1.0), Object::Real(0.5)]),
        );

        let doc = Document::new();
        let f = PdfFunction::from_dict(&dict, &doc).unwrap();
        let result = f.evaluate(0.5);
        assert!((result[0] - 0.5).abs() < 1e-5);
        assert!((result[1] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn parse_type2_defaults() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("FunctionType"), Object::Integer(2));
        dict.insert(PdfName::new("N"), Object::Integer(1));

        let doc = Document::new();
        let f = PdfFunction::from_dict(&dict, &doc).unwrap();
        assert!((f.evaluate(0.0)[0]).abs() < 1e-5);
        assert!((f.evaluate(1.0)[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn parse_unknown_type_returns_none() {
        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("FunctionType"), Object::Integer(99));

        let doc = Document::new();
        assert!(PdfFunction::from_dict(&dict, &doc).is_none());
    }

    // ---- Type 0 (Sampled) tests ----

    #[test]
    fn type0_1d_linear_lookup() {
        // 5 samples linearly spaced: [0, 64, 128, 191, 255] → [0.0, ~0.25, ~0.50, ~0.75, 1.0]
        let f = PdfFunction::Sampled {
            domain: [0.0, 1.0],
            range: vec![0.0, 1.0],
            size: 5,
            encode: [0.0, 4.0],
            decode: vec![0.0, 1.0],
            n_outputs: 1,
            samples: vec![0.0, 64.0 / 255.0, 128.0 / 255.0, 191.0 / 255.0, 1.0],
        };

        // At x=0.0 → sample[0] = 0.0
        assert!((f.evaluate(0.0)[0]).abs() < 1e-3);
        // At x=0.5 → encoded = 2.0 → sample[2] = 0.502
        assert!((f.evaluate(0.5)[0] - 128.0 / 255.0).abs() < 1e-3);
        // At x=1.0 → sample[4] = 1.0
        assert!((f.evaluate(1.0)[0] - 1.0).abs() < 1e-3);
        // At x=0.25 → encoded = 1.0 → sample[1] = 64/255
        assert!((f.evaluate(0.25)[0] - 64.0 / 255.0).abs() < 1e-3);
    }

    #[test]
    fn type0_clamps_input() {
        let f = PdfFunction::Sampled {
            domain: [0.0, 1.0],
            range: vec![0.0, 1.0],
            size: 3,
            encode: [0.0, 2.0],
            decode: vec![0.0, 1.0],
            n_outputs: 1,
            samples: vec![0.0, 0.5, 1.0],
        };

        // Clamped to domain[0] → 0.0
        assert!((f.evaluate(-0.5)[0]).abs() < 1e-5);
        // Clamped to domain[1] → 1.0
        assert!((f.evaluate(1.5)[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn type0_interpolates_between_samples() {
        let f = PdfFunction::Sampled {
            domain: [0.0, 1.0],
            range: vec![0.0, 1.0],
            size: 3,
            encode: [0.0, 2.0],
            decode: vec![0.0, 1.0],
            n_outputs: 1,
            samples: vec![0.0, 0.5, 1.0],
        };

        // x=0.25 → encoded=0.5 → lerp(sample[0], sample[1], 0.5) = 0.25
        assert!((f.evaluate(0.25)[0] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn type0_multi_output() {
        // 3 samples, 2 outputs each
        let f = PdfFunction::Sampled {
            domain: [0.0, 1.0],
            range: vec![0.0, 1.0, 0.0, 1.0],
            size: 3,
            encode: [0.0, 2.0],
            decode: vec![0.0, 1.0, 0.0, 1.0],
            n_outputs: 2,
            samples: vec![
                0.0, 1.0, // sample 0
                0.5, 0.5, // sample 1
                1.0, 0.0, // sample 2
            ],
        };

        let r = f.evaluate(0.0);
        assert_eq!(r.len(), 2);
        assert!((r[0]).abs() < 1e-5);
        assert!((r[1] - 1.0).abs() < 1e-5);

        let r = f.evaluate(1.0);
        assert!((r[0] - 1.0).abs() < 1e-5);
        assert!((r[1]).abs() < 1e-5);
    }

    #[test]
    fn parse_type0_from_stream() {
        use crate::core::objects::PdfStream;

        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("FunctionType"), Object::Integer(0));
        dict.insert(
            PdfName::new("Domain"),
            Object::Array(vec![Object::Real(0.0), Object::Real(1.0)]),
        );
        dict.insert(
            PdfName::new("Range"),
            Object::Array(vec![Object::Real(0.0), Object::Real(1.0)]),
        );
        dict.insert(
            PdfName::new("Size"),
            Object::Array(vec![Object::Integer(3)]),
        );
        dict.insert(PdfName::new("BitsPerSample"), Object::Integer(8));

        // 3 samples: 0, 128, 255
        let stream = PdfStream::new(dict, vec![0, 128, 255]);
        let doc = Document::new();
        let f = PdfFunction::from_object(&Object::Stream(stream), &doc).unwrap();

        assert!((f.evaluate(0.0)[0]).abs() < 1e-3);
        assert!((f.evaluate(0.5)[0] - 128.0 / 255.0).abs() < 1e-3);
        assert!((f.evaluate(1.0)[0] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn parse_type0_missing_size_returns_none() {
        use crate::core::objects::PdfStream;

        let mut dict = Dictionary::new();
        dict.insert(PdfName::new("FunctionType"), Object::Integer(0));
        dict.insert(PdfName::new("BitsPerSample"), Object::Integer(8));
        // No Size entry

        let stream = PdfStream::new(dict, vec![0, 128, 255]);
        let doc = Document::new();
        assert!(PdfFunction::from_object(&Object::Stream(stream), &doc).is_none());
    }
}
