//! Vector quantization primitives for the HNSW backend (H3).
//!
//! This module is always compiled (it has no optional dependencies). It
//! defines a small quantizer abstraction plus a cosine-distance metric that
//! operates directly on quantized vectors. The intended use is to reduce the
//! in-memory footprint of the HNSW graph: an 8-bit scalar quantizer stores one
//! byte per dimension instead of four, yielding a ~4x memory reduction for
//! the graph itself while leaving a full-f32 exact cache for final re-scoring.
//!
//! ## Quantizers
//!
//! - [`Scalar8BitQuantizer`] — per-corpus min/max 8-bit linear scalar
//!   quantization. Every component is encoded as `u8` and decoded back to
//!   the original range. This is the baseline quantizer used by the
//!   quantized HNSW backend.
//!
//! ## Metric
//!
//! [`QuantizedCosineDistance`] decodes two quantized vectors to f32 inside
//! the distance function, computes cosine similarity, and bit-casts the
//! distance to `u32` in the same way [`super::hnsw_backend::CosineDistance`]
//! does. The query vector is quantized with the same quantizer before calling
//! `nearest`, so distance comparisons are symmetric.

#[cfg(feature = "hnsw-backend")]
use std::fmt;

#[cfg(feature = "hnsw-backend")]
use super::vector::cosine_similarity;

#[cfg(feature = "hnsw-backend")]
use space::Metric;

/// A vector quantizer that can encode and decode vectors.
///
/// Implementations are expected to be deterministic and stateless (apart
/// from their trained parameters) so the same quantizer can be used for
/// both index construction and query-time encoding.
pub trait Quantizer: Send + Sync + 'static {
    /// Encode a dense f32 vector into bytes.
    fn encode(&self, vector: &[f32]) -> Vec<u8>;

    /// Decode a byte vector back into f32.
    fn decode(&self, encoded: &[u8]) -> Vec<f32>;
}

/// 8-bit scalar quantizer.
///
/// Computes a global minimum and maximum over a training corpus, then maps
/// each component linearly into `[0, 255]`. Decoding restores the f32 range
/// with uniform de-quantization. The quantizer is one byte per component,
/// so it reduces HNSW graph memory usage by 4x relative to raw f32 storage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scalar8BitQuantizer {
    /// Global minimum value observed during training.
    min: f32,
    /// Global maximum value observed during training.
    max: f32,
    /// Precomputed `1.0 / (max - min)` to avoid division at runtime.
    inv_range: f32,
}

impl Scalar8BitQuantizer {
    /// Training fallback range when the corpus is empty. Vectors seen later
    /// will be encoded relative to `[-1.0, 1.0]`.
    fn empty_fallback() -> Self {
        Self {
            min: -1.0,
            max: 1.0,
            inv_range: 0.5,
        }
    }

    /// Train a quantizer by scanning all components of all training vectors.
    ///
    /// Returns [`Self::empty_fallback`] when no vectors are supplied.
    pub fn train(vectors: &[Vec<f32>]) -> Self {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for vector in vectors {
            for &value in vector {
                if value < min {
                    min = value;
                }
                if value > max {
                    max = value;
                }
            }
        }
        if !min.is_finite() || !max.is_finite() || min == max {
            return Self::empty_fallback();
        }
        let range = max - min;
        Self {
            min,
            max,
            inv_range: 1.0 / range,
        }
    }

    /// Encode one vector component into a byte.
    fn encode_value(&self, value: f32) -> u8 {
        if !value.is_finite() {
            return 0;
        }
        let normalized = (value - self.min) * self.inv_range;
        let clamped = normalized.clamp(0.0, 1.0);
        (clamped * 255.0).round() as u8
    }

    /// Decode one byte back into f32.
    fn decode_value(&self, byte: u8) -> f32 {
        self.min + (byte as f32 / 255.0) * (self.max - self.min)
    }
}

impl Quantizer for Scalar8BitQuantizer {
    fn encode(&self, vector: &[f32]) -> Vec<u8> {
        vector.iter().map(|&v| self.encode_value(v)).collect()
    }

    fn decode(&self, encoded: &[u8]) -> Vec<f32> {
        encoded.iter().map(|&b| self.decode_value(b)).collect()
    }
}

/// A quantized vector stored as raw bytes.
#[cfg(feature = "hnsw-backend")]
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QuantizedVector(pub Vec<u8>);

#[cfg(feature = "hnsw-backend")]
impl QuantizedVector {
    /// Wrap a byte buffer.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

#[cfg(feature = "hnsw-backend")]
impl fmt::Debug for QuantizedVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("QuantizedVector")
            .field(&format!("{} bytes", self.0.len()))
            .finish()
    }
}

/// Cosine-distance metric for quantized vectors.
///
/// Decodes both operands with the supplied quantizer, computes cosine
/// similarity, and returns the bit-cast of `1.0 - similarity` in `[0.0, 2.0]`
/// as a `u32`. See [`super::hnsw_backend::CosineDistance`] for the rationale.
#[cfg(feature = "hnsw-backend")]
#[derive(Debug, Clone, Copy)]
pub struct QuantizedCosineDistance {
    quantizer: Scalar8BitQuantizer,
}

#[cfg(feature = "hnsw-backend")]
impl QuantizedCosineDistance {
    /// Create a metric from a trained quantizer.
    pub fn new(quantizer: Scalar8BitQuantizer) -> Self {
        Self { quantizer }
    }
}

#[cfg(feature = "hnsw-backend")]
impl Metric<QuantizedVector> for QuantizedCosineDistance {
    type Unit = u32;

    fn distance(&self, a: &QuantizedVector, b: &QuantizedVector) -> Self::Unit {
        // Same uniform integer scaling as the f32 HNSW backend so both
        // backends share comparable greedy-search behavior.
        const DISTANCE_SCALE: f32 = 1_000_000.0;
        let left = self.quantizer.decode(&a.0);
        let right = self.quantizer.decode(&b.0);
        let similarity = cosine_similarity(&left, &right).unwrap_or(0.0);
        let distance = (1.0_f32 - similarity).clamp(0.0, 2.0);
        (distance * DISTANCE_SCALE) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_8_bit_round_trip_error_is_bounded() {
        let vectors: Vec<Vec<f32>> = (0..100)
            .map(|i| {
                (0..32)
                    .map(|d| ((i * 7 + d * 13) % 200) as f32 / 100.0 - 1.0)
                    .collect()
            })
            .collect();
        let quantizer = Scalar8BitQuantizer::train(&vectors);
        let reconstructed: Vec<Vec<f32>> = vectors
            .iter()
            .map(|v| quantizer.decode(&quantizer.encode(v)))
            .collect();

        let mut max_abs_error: f32 = 0.0;
        for (orig, recon) in vectors.iter().zip(reconstructed.iter()) {
            for (o, r) in orig.iter().zip(recon.iter()) {
                let err = (o - r).abs();
                if err > max_abs_error {
                    max_abs_error = err;
                }
            }
        }
        // 8-bit uniform quantization over the observed range has a worst-case
        // per-component error of (range / 255) / 2.
        assert!(
            max_abs_error <= (quantizer.max - quantizer.min) / 255.0,
            "max_abs_error {max_abs_error} exceeded quantization bucket size"
        );
    }

    #[test]
    #[cfg(feature = "hnsw-backend")]
    fn quantized_cosine_distance_preserves_order() {
        let vectors: Vec<Vec<f32>> = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![-1.0, 0.0]];
        let quantizer = Scalar8BitQuantizer::train(&vectors);
        let metric = QuantizedCosineDistance::new(quantizer);

        let a = QuantizedVector(quantizer.encode(&[1.0, 0.0]));
        let close = QuantizedVector(quantizer.encode(&[0.9, 0.1]));
        let far = QuantizedVector(quantizer.encode(&[-0.9, 0.1]));

        let d_close = metric.distance(&a, &close);
        let d_far = metric.distance(&a, &far);
        assert!(
            d_close < d_far,
            "quantized distances should preserve similarity order"
        );
    }
}
