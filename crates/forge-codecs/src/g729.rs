//! G.729 codec implementation
//!
//! G.729 is a narrow-band speech codec standardized by ITU-T that compresses
//! 16-bit PCM audio at 8 kHz to 8 kbit/s using CS-ACELP (Conjugate-Structure
//! Algebraic-Code-Excited Linear-Prediction).
//!
//! Key features:
//! - Sample rate: 8 kHz
//! - Bit rate: 8 kbit/s (G.729) or 11.8 kbit/s (G.729 Annex A)
//! - Frame size: 10 ms (80 samples)
//! - Algorithmic delay: 15 ms
//! - Patents expired in 2017 - freely usable
//!
//! This implementation follows ITU-T G.729 specification and uses
//! fixed-point arithmetic (Q15/Q31 format) as specified.

use crate::AudioCodec;
use crate::{AudioFormat, CodecError, Result};

// =============================================================================
// Fixed-Point Arithmetic (ITU-T G.729 Basic Operations)
// =============================================================================
//
// G.729 uses fixed-point arithmetic in Q15 and Q31 formats:
// - Q15: 16-bit signed, range [-1.0, 0.99997]
// - Q31: 32-bit signed, range [-1.0, 0.99999999]
//
// All operations saturate on overflow per ITU specification.

/// Fixed-point constants
const MAX_16: i16 = 0x7FFF;  // 32767
const MIN_16: i16 = -0x8000; // -32768
const MAX_32: i32 = 0x7FFF_FFFF;  // 2147483647
const MIN_32: i32 = -0x8000_0000; // -2147483648

/// Saturated 16-bit addition (Q15 format)
#[inline]
fn sat_add(a: i16, b: i16) -> i16 {
    let sum = a as i32 + b as i32;
    sum.clamp(MIN_16 as i32, MAX_16 as i32) as i16
}

/// Saturated 16-bit subtraction (Q15 format)
#[inline]
fn sat_sub(a: i16, b: i16) -> i16 {
    let diff = a as i32 - b as i32;
    diff.clamp(MIN_16 as i32, MAX_16 as i32) as i16
}

/// Saturated 16-bit multiplication (Q15 × Q15 → Q15)
/// Result = (a * b) >> 15, saturated
#[inline]
fn sat_mult(a: i16, b: i16) -> i16 {
    let product = (a as i32 * b as i32) >> 15;
    product.clamp(MIN_16 as i32, MAX_16 as i32) as i16
}

/// 32-bit multiply with left shift (Q15 × Q15 → Q31)
/// L_mult(a, b) = (a * b) << 1, saturated to 32-bit
/// Special case: L_mult(-32768, -32768) = MAX_32
#[inline]
fn l_mult(a: i16, b: i16) -> i32 {
    if a == MIN_16 && b == MIN_16 {
        MAX_32
    } else {
        let product = (a as i64 * b as i64) << 1;
        product.clamp(MIN_32 as i64, MAX_32 as i64) as i32
    }
}

/// Saturated 32-bit addition (Q31 format)
#[inline]
fn l_add(a: i32, b: i32) -> i32 {
    a.saturating_add(b)
}

/// Saturated 32-bit subtraction (Q31 format)
#[inline]
fn l_sub(a: i32, b: i32) -> i32 {
    a.saturating_sub(b)
}

/// Arithmetic left shift with saturation (16-bit)
#[inline]
fn shl(val: i16, shift: i32) -> i16 {
    if shift <= 0 {
        return val >> (-shift).min(15);
    }
    let shifted = (val as i32) << shift.min(15);
    shifted.clamp(MIN_16 as i32, MAX_16 as i32) as i16
}

/// Arithmetic right shift (16-bit)
#[inline]
fn shr(val: i16, shift: i32) -> i16 {
    if shift <= 0 {
        return shl(val, -shift);
    }
    val >> shift.min(15)
}

/// Arithmetic left shift with saturation (32-bit)
#[inline]
fn l_shl(val: i32, shift: i32) -> i32 {
    if shift <= 0 {
        return val >> (-shift).min(31);
    }
    let shifted = (val as i64) << shift.min(31);
    shifted.clamp(MIN_32 as i64, MAX_32 as i64) as i32
}

/// Arithmetic right shift (32-bit)
#[inline]
fn l_shr(val: i32, shift: i32) -> i32 {
    if shift <= 0 {
        return l_shl(val, -shift);
    }
    val >> shift.min(31)
}

/// Absolute value with saturation (16-bit)
/// Special case: abs(-32768) = 32767
#[inline]
fn sat_abs(val: i16) -> i16 {
    if val == MIN_16 {
        MAX_16
    } else {
        val.abs()
    }
}

/// Negate with saturation (16-bit)
/// Special case: -(-32768) = 32767
#[inline]
fn sat_negate(val: i16) -> i16 {
    if val == MIN_16 {
        MAX_16
    } else {
        -val
    }
}

/// G.729 frame size in samples (10 ms at 8 kHz)
pub const FRAME_SIZE: usize = 80;

/// G.729 compressed frame size in bytes
pub const ENCODED_FRAME_SIZE: usize = 10;

/// G.729 sample rate
pub const SAMPLE_RATE: u32 = 8000;

/// G.729 codec variants
///
/// Note: bcg729 implements G.729 Annex A (the reduced-complexity version),
/// which is what most implementations use. All variants produce 10-byte frames
/// for speech. The difference is in VAD (Voice Activity Detection):
/// - G729/G729A: Always 10-byte frames
/// - G729B: 10-byte speech, 2-byte SID (silence), or 0-byte untransmitted
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum G729Variant {
    /// G.729 Annex A - Standard 8 kbit/s codec (what bcg729 implements)
    G729A,
    /// G.729 Annex B - With Voice Activity Detection (VAD)
    G729B,
}

impl G729Variant {
    /// Get bit rate for this variant (nominal, excluding VAD savings)
    pub fn bit_rate(&self) -> u32 {
        match self {
            G729Variant::G729A => 8000,
            G729Variant::G729B => 8000, // Variable with VAD, can be lower
        }
    }

    /// Get maximum encoded frame size in bytes
    /// - G729A: Always 10 bytes
    /// - G729B: 10 bytes (speech), 2 bytes (SID), or 0 bytes (untransmitted)
    pub fn max_frame_size(&self) -> usize {
        10  // Both variants can produce up to 10-byte frames
    }

    /// Whether this variant supports Voice Activity Detection
    pub fn has_vad(&self) -> bool {
        matches!(self, G729Variant::G729B)
    }
}

/// G.729 encoder state
struct G729EncoderState {
    /// Pre-processing state (high-pass filter)
    preproc_state: [i16; 2],
    /// Linear Prediction Coding state
    lpc_state: LpcState,
    /// Pitch predictor state
    pitch_state: PitchState,
    /// Codebook state
    codebook_state: CodebookState,
    /// Previous frame energy for VAD
    prev_energy: f32,
}

/// G.729 decoder state
struct G729DecoderState {
    /// Post-processing state
    postproc_state: [i16; 2],
    /// Synthesis filter state
    synthesis_state: [i16; 10],
    /// Pitch synthesis state
    pitch_synth_state: PitchState,
    /// Previous frame for error concealment
    prev_lsf: [i16; 10],
}

/// Linear Prediction Coding state
struct LpcState {
    /// Previous LPC coefficients
    prev_lpc: [i16; 10],
    /// Line Spectral Frequencies
    lsf: [i16; 10],
    /// Quantized LSF
    qlsf: [i16; 10],
}

/// Pitch predictor state
struct PitchState {
    /// Previous pitch delays
    prev_delay: [i16; 5],
    /// Previous pitch gains
    prev_gain: [i16; 4],
    /// Excitation buffer
    excitation: [i16; 256],
}

/// Algebraic codebook state
struct CodebookState {
    /// Previous codebook indices
    prev_indices: [u8; 4],
    /// Previous gains
    prev_gains: [i16; 2],
}

impl G729EncoderState {
    fn new() -> Self {
        Self {
            preproc_state: [0; 2],
            lpc_state: LpcState::new(),
            pitch_state: PitchState::new(),
            codebook_state: CodebookState::new(),
            prev_energy: 0.0,
        }
    }

    fn reset(&mut self) {
        self.preproc_state = [0; 2];
        self.lpc_state.reset();
        self.pitch_state.reset();
        self.codebook_state.reset();
        self.prev_energy = 0.0;
    }
}

impl G729DecoderState {
    fn new() -> Self {
        Self {
            postproc_state: [0; 2],
            synthesis_state: [0; 10],
            pitch_synth_state: PitchState::new(),
            prev_lsf: [0; 10],
        }
    }

    fn reset(&mut self) {
        self.postproc_state = [0; 2];
        self.synthesis_state = [0; 10];
        self.pitch_synth_state.reset();
        self.prev_lsf = [0; 10];
    }
}

impl LpcState {
    fn new() -> Self {
        Self {
            prev_lpc: [0; 10],
            lsf: [0; 10],
            qlsf: [0; 10],
        }
    }

    fn reset(&mut self) {
        self.prev_lpc = [0; 10];
        self.lsf = [0; 10];
        self.qlsf = [0; 10];
    }
}

impl PitchState {
    fn new() -> Self {
        Self {
            prev_delay: [40; 5], // Initialize with typical pitch period
            prev_gain: [0; 4],
            excitation: [0; 256],
        }
    }

    fn reset(&mut self) {
        self.prev_delay = [40; 5];
        self.prev_gain = [0; 4];
        self.excitation = [0; 256];
    }
}

impl CodebookState {
    fn new() -> Self {
        Self {
            prev_indices: [0; 4],
            prev_gains: [0; 2],
        }
    }

    fn reset(&mut self) {
        self.prev_indices = [0; 4];
        self.prev_gains = [0; 2];
    }
}

// =============================================================================
// Safe Rust Wrappers for bcg729 FFI
// =============================================================================

#[cfg(feature = "g729")]
mod bcg729_wrapper {
    use super::*;
    use bcg729_sys::*;
    use std::ptr::NonNull;

    /// Safe wrapper around bcg729 encoder
    pub struct G729Encoder {
        ctx: NonNull<bcg729EncoderChannelContextStruct>,
        enable_vad: bool,
    }

    impl G729Encoder {
        /// Create a new G.729 encoder
        ///
        /// # Parameters
        /// - `enable_vad`: Enable Voice Activity Detection (Annex B)
        pub fn new(enable_vad: bool) -> Result<Self> {
            unsafe {
                let ctx = initBcg729EncoderChannel(if enable_vad { 1 } else { 0 });
                let ctx = NonNull::new(ctx).ok_or_else(|| {
                    CodecError::InitializationFailed("Failed to initialize bcg729 encoder".into())
                })?;
                Ok(Self { ctx, enable_vad })
            }
        }

        /// Encode one frame (80 samples = 10ms) to G.729
        ///
        /// # Parameters
        /// - `pcm`: 80 PCM samples (16-bit signed, 8kHz)
        ///
        /// # Returns
        /// - Normal frame: 10 bytes
        /// - SID frame (if VAD enabled): 2 bytes
        /// - Untransmitted (if VAD enabled): 0 bytes
        pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
            if pcm.len() != FRAME_SIZE {
                return Err(CodecError::InvalidFormat(format!(
                    "G.729 encoder requires exactly {} samples, got {}",
                    FRAME_SIZE,
                    pcm.len()
                )));
            }

            unsafe {
                let mut output = vec![0u8; 10]; // Maximum output size
                let mut output_len: u8 = 0;

                bcg729Encoder(
                    self.ctx.as_ptr(),
                    pcm.as_ptr(),
                    output.as_mut_ptr(),
                    &mut output_len as *mut u8,
                );

                output.truncate(output_len as usize);
                Ok(output)
            }
        }

        /// Get RFC 3389 Comfort Noise payload (11 bytes)
        ///
        /// Only valid if VAD is enabled and a SID frame was just encoded.
        pub fn get_rfc3389_payload(&self) -> Result<[u8; 11]> {
            if !self.enable_vad {
                return Err(CodecError::InvalidFormat(
                    "RFC 3389 payload only available with VAD enabled".into(),
                ));
            }

            unsafe {
                let mut payload = [0u8; 11];
                bcg729GetRFC3389Payload(self.ctx.as_ptr(), payload.as_mut_ptr());
                Ok(payload)
            }
        }
    }

    impl Drop for G729Encoder {
        fn drop(&mut self) {
            unsafe {
                closeBcg729EncoderChannel(self.ctx.as_ptr());
            }
        }
    }

    // Safety: bcg729 encoder is thread-safe per instance
    unsafe impl Send for G729Encoder {}
    unsafe impl Sync for G729Encoder {}

    /// Safe wrapper around bcg729 decoder
    pub struct G729Decoder {
        ctx: NonNull<bcg729DecoderChannelContextStruct>,
    }

    impl G729Decoder {
        /// Create a new G.729 decoder
        pub fn new() -> Result<Self> {
            unsafe {
                let ctx = initBcg729DecoderChannel();
                let ctx = NonNull::new(ctx).ok_or_else(|| {
                    CodecError::InitializationFailed("Failed to initialize bcg729 decoder".into())
                })?;
                Ok(Self { ctx })
            }
        }

        /// Decode one G.729 frame to PCM samples
        ///
        /// # Parameters
        /// - `data`: Encoded data (10 bytes for normal frame, 2 for SID, 0 for erasure)
        /// - `erasure`: Set to true if frame was lost (packet loss concealment)
        ///
        /// # Returns
        /// 80 PCM samples (16-bit signed, 8kHz)
        pub fn decode(&mut self, data: &[u8], erasure: bool) -> Result<Vec<i16>> {
            let data_len = data.len();

            // Validate frame size
            if !erasure && data_len != 10 && data_len != 2 && data_len != 0 {
                return Err(CodecError::InvalidFormat(format!(
                    "G.729 decoder requires 0, 2, or 10 bytes, got {}",
                    data_len
                )));
            }

            unsafe {
                let mut output = vec![0i16; FRAME_SIZE];

                // Determine frame type
                let (bit_stream, bit_stream_len, sid_flag) = if data_len == 2 {
                    // SID frame (comfort noise)
                    (data.as_ptr(), 2u8, 1u8)
                } else if data_len == 0 || erasure {
                    // Frame erasure (packet loss)
                    (std::ptr::null(), 0u8, 0u8)
                } else {
                    // Normal frame
                    (data.as_ptr(), 10u8, 0u8)
                };

                bcg729Decoder(
                    self.ctx.as_ptr(),
                    bit_stream,
                    bit_stream_len,
                    if erasure { 1 } else { 0 }, // frameErasureFlag
                    sid_flag,                     // SIDFrameFlag
                    0,                            // rfc3389PayloadFlag (not used for now)
                    output.as_mut_ptr(),
                );

                Ok(output)
            }
        }

        /// Decode with RFC 3389 Comfort Noise payload
        ///
        /// # Parameters
        /// - `cn_payload`: 11-byte RFC 3389 CN payload
        pub fn decode_rfc3389(&mut self, cn_payload: &[u8]) -> Result<Vec<i16>> {
            if cn_payload.len() != 11 {
                return Err(CodecError::InvalidFormat(format!(
                    "RFC 3389 payload must be 11 bytes, got {}",
                    cn_payload.len()
                )));
            }

            unsafe {
                let mut output = vec![0i16; FRAME_SIZE];

                bcg729Decoder(
                    self.ctx.as_ptr(),
                    cn_payload.as_ptr(),
                    11,
                    0, // frameErasureFlag
                    1, // SIDFrameFlag
                    1, // rfc3389PayloadFlag
                    output.as_mut_ptr(),
                );

                Ok(output)
            }
        }
    }

    impl Drop for G729Decoder {
        fn drop(&mut self) {
            unsafe {
                closeBcg729DecoderChannel(self.ctx.as_ptr());
            }
        }
    }

    // Safety: bcg729 decoder is thread-safe per instance
    unsafe impl Send for G729Decoder {}
    unsafe impl Sync for G729Decoder {}
}

#[cfg(feature = "g729")]
use bcg729_wrapper::{G729Decoder as Bcg729Decoder, G729Encoder as Bcg729Encoder};

/// G.729 codec
pub struct G729Codec {
    variant: G729Variant,
    #[cfg(feature = "g729")]
    encoder: Option<Bcg729Encoder>,
    #[cfg(feature = "g729")]
    decoder: Option<Bcg729Decoder>,
    #[cfg(not(feature = "g729"))]
    encoder_state: G729EncoderState,
    #[cfg(not(feature = "g729"))]
    decoder_state: G729DecoderState,
}

impl G729Codec {
    /// Create a new G.729 codec with standard variant (G.729A)
    pub fn new() -> Result<Self> {
        Self::new_with_variant(G729Variant::G729A)
    }

    /// Create a new G.729 codec with specified variant
    pub fn new_with_variant(variant: G729Variant) -> Result<Self> {
        #[cfg(feature = "g729")]
        {
            // VAD is enabled for G.729 Annex B
            let enable_vad = variant.has_vad();
            let encoder = Bcg729Encoder::new(enable_vad)?;
            let decoder = Bcg729Decoder::new()?;
            Ok(Self {
                variant,
                encoder: Some(encoder),
                decoder: Some(decoder),
            })
        }
        #[cfg(not(feature = "g729"))]
        {
            Ok(Self {
                variant,
                encoder_state: G729EncoderState::new(),
                decoder_state: G729DecoderState::new(),
            })
        }
    }

    /// Encode a frame of PCM samples to G.729
    fn encode_frame(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        if pcm.len() != FRAME_SIZE {
            return Err(CodecError::InvalidFormat(format!(
                "G.729 requires exactly {} samples per frame, got {}",
                FRAME_SIZE,
                pcm.len()
            )));
        }

        #[cfg(feature = "g729")]
        {
            if let Some(encoder) = &mut self.encoder {
                return encoder.encode(pcm);
            } else {
                return Err(CodecError::InitializationFailed(
                    "G.729 encoder not initialized".into(),
                ));
            }
        }

        #[cfg(not(feature = "g729"))]
        {
            // Stub implementation - not functional
            // Step 1: Pre-processing (high-pass filter)
            let preprocessed = self.preprocess(pcm);

            // Step 2: Linear Prediction Analysis
            let lpc_coeffs = self.lpc_analysis(&preprocessed);

            // Step 3: Convert to Line Spectral Frequencies (LSF)
            let lsf = self.lpc_to_lsf(&lpc_coeffs);

            // Step 4: Quantize LSF
            let qlsf = self.quantize_lsf(&lsf);

            // Step 5: Compute residual signal
            let residual = self.compute_residual(&preprocessed, &lpc_coeffs);

            // Step 6: Pitch analysis (adaptive codebook search)
            let (pitch_delay, pitch_gain) = self.pitch_analysis(&residual);

            // Step 7: Algebraic codebook search
            let (cb_indices, cb_gains) = self.codebook_search(&residual, pitch_delay, pitch_gain);

            // Step 8: Pack parameters into bitstream
            let encoded = self.pack_bitstream(qlsf, pitch_delay, pitch_gain, cb_indices, cb_gains);

            Ok(encoded)
        }
    }

    /// Decode a frame of G.729 to PCM samples
    fn decode_frame(&mut self, encoded: &[u8]) -> Result<Vec<i16>> {
        #[cfg(feature = "g729")]
        {
            if let Some(decoder) = &mut self.decoder {
                // bcg729 handles frame size validation
                return decoder.decode(encoded, false);
            } else {
                return Err(CodecError::InitializationFailed(
                    "G.729 decoder not initialized".into(),
                ));
            }
        }

        #[cfg(not(feature = "g729"))]
        {
            if encoded.len() != self.variant.frame_size() {
                return Err(CodecError::InvalidFormat(format!(
                    "G.729 requires exactly {} bytes per frame, got {}",
                    self.variant.frame_size(),
                    encoded.len()
                )));
            }

            // Stub implementation - not functional
            // Step 1: Unpack bitstream
            let params = self.unpack_bitstream(encoded)?;

            // Step 2: Decode LSF to LPC coefficients
            let lpc_coeffs = self.lsf_to_lpc(&params.lsf);

            // Step 3: Reconstruct excitation signal
            let excitation = self.reconstruct_excitation(
                params.pitch_delay,
                params.pitch_gain,
                &params.codebook_indices,
                &params.codebook_gains,
            );

            // Step 4: Synthesis filtering
            let synthesized = self.synthesis_filter(&excitation, &lpc_coeffs);

            // Step 5: Post-processing
            let output = self.postprocess(&synthesized);

            Ok(output)
        }
    }

    // Placeholder implementations for codec stages (only used in stub mode)
    #[cfg(not(feature = "g729"))]
    fn preprocess(&mut self, _pcm: &[i16]) -> Vec<i16> {
        // TODO: Implement high-pass filter (80 Hz cutoff)
        vec![0; FRAME_SIZE]
    }

    #[cfg(not(feature = "g729"))]
    fn lpc_analysis(&mut self, _signal: &[i16]) -> Vec<f32> {
        // TODO: Implement 10th order LPC analysis using autocorrelation method
        vec![0.0; 10]
    }

    #[cfg(not(feature = "g729"))]
    fn lpc_to_lsf(&mut self, _lpc: &[f32]) -> Vec<i16> {
        // TODO: Convert LPC coefficients to Line Spectral Frequencies
        vec![0; 10]
    }

    #[cfg(not(feature = "g729"))]
    fn quantize_lsf(&mut self, _lsf: &[i16]) -> Vec<u8> {
        // TODO: Quantize LSF using split vector quantization
        vec![0; 4]
    }

    #[cfg(not(feature = "g729"))]
    fn compute_residual(&self, _signal: &[i16], _lpc: &[f32]) -> Vec<i16> {
        // TODO: Compute residual by inverse filtering
        vec![0; FRAME_SIZE]
    }

    #[cfg(not(feature = "g729"))]
    fn pitch_analysis(&mut self, _residual: &[i16]) -> (i16, i16) {
        // TODO: Adaptive codebook search (pitch prediction)
        (40, 0) // delay, gain
    }

    #[cfg(not(feature = "g729"))]
    fn codebook_search(
        &mut self,
        _residual: &[i16],
        _delay: i16,
        _gain: i16,
    ) -> (Vec<u8>, Vec<i16>) {
        // TODO: Algebraic codebook search
        (vec![0; 4], vec![0; 2]) // indices, gains
    }

    #[cfg(not(feature = "g729"))]
    fn pack_bitstream(
        &self,
        _lsf: Vec<u8>,
        _delay: i16,
        _gain: i16,
        _indices: Vec<u8>,
        _gains: Vec<i16>,
    ) -> Vec<u8> {
        // TODO: Pack parameters into 80-bit frame
        vec![0; ENCODED_FRAME_SIZE]
    }

    #[cfg(not(feature = "g729"))]
    fn unpack_bitstream(&self, _encoded: &[u8]) -> Result<DecodedParams> {
        // TODO: Unpack 80-bit frame into parameters
        Ok(DecodedParams {
            lsf: vec![0; 10],
            pitch_delay: 40,
            pitch_gain: 0,
            codebook_indices: vec![0; 4],
            codebook_gains: vec![0; 2],
        })
    }

    #[cfg(not(feature = "g729"))]
    fn lsf_to_lpc(&mut self, _lsf: &[i16]) -> Vec<f32> {
        // TODO: Convert LSF back to LPC coefficients
        vec![0.0; 10]
    }

    #[cfg(not(feature = "g729"))]
    fn reconstruct_excitation(
        &mut self,
        _delay: i16,
        _gain: i16,
        _indices: &[u8],
        _gains: &[i16],
    ) -> Vec<i16> {
        // TODO: Reconstruct excitation from pitch and codebook
        vec![0; FRAME_SIZE]
    }

    #[cfg(not(feature = "g729"))]
    fn synthesis_filter(&mut self, _excitation: &[i16], _lpc: &[f32]) -> Vec<i16> {
        // TODO: Synthesis filtering
        vec![0; FRAME_SIZE]
    }

    #[cfg(not(feature = "g729"))]
    fn postprocess(&mut self, _signal: &[i16]) -> Vec<i16> {
        // TODO: Post-processing
        vec![0; FRAME_SIZE]
    }

    // =======================================================================
    // Public frame-by-frame APIs for advanced use cases
    // =======================================================================

    /// Encode a single frame (80 samples) to G.729
    ///
    /// Returns variable-length output:
    /// - 10 bytes: Normal speech frame
    /// - 2 bytes: SID frame (comfort noise, if VAD enabled)
    /// - 0 bytes: Untransmitted frame (silence, if VAD enabled)
    ///
    /// For proper framing with VAD, use encode_frame_framed() instead.
    #[cfg(feature = "g729")]
    pub fn encode_frame_unframed(&mut self, pcm: &[i16; 80]) -> Result<Vec<u8>> {
        if let Some(encoder) = &mut self.encoder {
            encoder.encode(pcm)
        } else {
            Err(CodecError::InitializationFailed(
                "G.729 encoder not initialized".into(),
            ))
        }
    }

    /// Decode a single G.729 frame to PCM samples
    ///
    /// Accepts:
    /// - 10 bytes: Normal frame
    /// - 2 bytes: SID frame
    /// - 0 bytes: Frame erasure (packet loss)
    ///
    /// # Parameters
    /// - `data`: Encoded frame data
    /// - `is_erasure`: Set to true for packet loss concealment
    #[cfg(feature = "g729")]
    pub fn decode_frame_with_plc(&mut self, data: &[u8], is_erasure: bool) -> Result<Vec<i16>> {
        if let Some(decoder) = &mut self.decoder {
            decoder.decode(data, is_erasure)
        } else {
            Err(CodecError::InitializationFailed(
                "G.729 decoder not initialized".into(),
            ))
        }
    }
}

/// Decoded frame parameters
struct DecodedParams {
    lsf: Vec<i16>,
    pitch_delay: i16,
    pitch_gain: i16,
    codebook_indices: Vec<u8>,
    codebook_gains: Vec<i16>,
}

// Note: Default trait not implemented because G729Codec::new() can fail.
// Use G729Codec::new() or G729Codec::new_with_variant() instead.

impl AudioCodec for G729Codec {
    fn name(&self) -> &str {
        match self.variant {
            G729Variant::G729A => "G.729A",
            G729Variant::G729B => "G.729B",
        }
    }

    fn native_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: SAMPLE_RATE,
            channels: 1,
            codec: crate::AudioCodecType::G729,
        }
    }

    fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        // Process in 10ms frames with length-prefix framing
        if pcm.len() % FRAME_SIZE != 0 {
            return Err(CodecError::InvalidFormat(format!(
                "G.729 input must be multiple of {} samples",
                FRAME_SIZE
            )));
        }

        #[cfg(feature = "g729")]
        {
            // Use length-prefix framing to handle variable-length frames
            // Format: [len:u8][data:len bytes][len:u8][data:len bytes]...
            // This allows VAD to produce 0, 2, or 10 byte frames unambiguously
            let mut encoded = Vec::new();
            for frame in pcm.chunks(FRAME_SIZE) {
                let frame_data = self.encode_frame(frame)?;
                let len = frame_data.len() as u8;
                encoded.push(len);  // 1-byte length prefix
                encoded.extend_from_slice(&frame_data);
            }
            Ok(encoded)
        }

        #[cfg(not(feature = "g729"))]
        {
            // Stub implementation - fixed 10-byte frames
            let mut encoded = Vec::new();
            for frame in pcm.chunks(FRAME_SIZE) {
                encoded.extend_from_slice(&self.encode_frame(frame)?);
            }
            Ok(encoded)
        }
    }

    fn decode(&mut self, encoded: &[u8]) -> Result<Vec<i16>> {
        #[cfg(feature = "g729")]
        {
            // Parse length-prefixed framed format
            // Format: [len:u8][data:len bytes][len:u8][data:len bytes]...
            if encoded.is_empty() {
                return Ok(Vec::new());
            }

            let mut decoded = Vec::new();
            let mut offset = 0;

            while offset < encoded.len() {
                // Read length prefix
                if offset >= encoded.len() {
                    return Err(CodecError::InvalidFormat(
                        "Truncated G.729 frame: missing length prefix".into(),
                    ));
                }

                let frame_len = encoded[offset] as usize;
                offset += 1;

                // Validate frame length
                if frame_len > 10 {
                    return Err(CodecError::InvalidFormat(format!(
                        "Invalid G.729 frame length: {} bytes (max 10)",
                        frame_len
                    )));
                }

                // Check we have enough data
                if offset + frame_len > encoded.len() {
                    return Err(CodecError::InvalidFormat(format!(
                        "Truncated G.729 frame: expected {} bytes, got {}",
                        frame_len,
                        encoded.len() - offset
                    )));
                }

                // Decode frame
                let frame = &encoded[offset..offset + frame_len];
                decoded.extend_from_slice(&self.decode_frame(frame)?);
                offset += frame_len;
            }

            Ok(decoded)
        }

        #[cfg(not(feature = "g729"))]
        {
            // Stub: assume fixed 10-byte frames
            if encoded.len() % 10 != 0 {
                return Err(CodecError::InvalidFormat(format!(
                    "G.729 encoded data must be multiple of 10 bytes, got {}",
                    encoded.len()
                )));
            }

            let mut decoded = Vec::new();
            for frame in encoded.chunks(10) {
                decoded.extend_from_slice(&self.decode_frame(frame)?);
            }
            Ok(decoded)
        }
    }

    fn frame_size(&self) -> Option<usize> {
        Some(FRAME_SIZE)
    }

    fn reset(&mut self) {
        #[cfg(feature = "g729")]
        {
            // Recreate encoder/decoder to reset state
            // Note: If recreation fails, we keep the old ones rather than panic
            let enable_vad = self.variant.has_vad();
            if let Ok(encoder) = Bcg729Encoder::new(enable_vad) {
                self.encoder = Some(encoder);
            }
            if let Ok(decoder) = Bcg729Decoder::new() {
                self.decoder = Some(decoder);
            }
        }
        #[cfg(not(feature = "g729"))]
        {
            self.encoder_state.reset();
            self.decoder_state.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "g729")]
    fn test_g729_create() {
        let codec = G729Codec::new().unwrap();
        assert_eq!(codec.name(), "G.729A");
        assert_eq!(codec.frame_size(), Some(80));
    }

    #[test]
    fn test_g729_variant_bitrates() {
        assert_eq!(G729Variant::G729A.bit_rate(), 8000);
        assert_eq!(G729Variant::G729B.bit_rate(), 8000);
    }

    #[test]
    fn test_g729_variant_properties() {
        assert_eq!(G729Variant::G729A.max_frame_size(), 10);
        assert_eq!(G729Variant::G729B.max_frame_size(), 10);
        assert!(!G729Variant::G729A.has_vad());
        assert!(G729Variant::G729B.has_vad());
    }

    #[test]
    #[cfg(feature = "g729")]
    fn test_g729_encode_decode_basic() {
        let mut codec = G729Codec::new().unwrap();
        let pcm = vec![100i16; 80]; // One frame

        let encoded = codec.encode(&pcm).unwrap();
        // Framed format: 1-byte length + frame data
        assert_eq!(encoded.len(), 11); // 1 (length) + 10 (frame)

        let decoded = codec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 80);
    }

    #[test]
    #[cfg(feature = "g729")]
    fn test_g729_encode_decode_silence() {
        let mut codec = G729Codec::new().unwrap();
        let pcm = vec![0i16; 80]; // Silence frame

        let encoded = codec.encode(&pcm).unwrap();
        assert_eq!(encoded.len(), 11); // 1 (length) + 10 (frame)

        let decoded = codec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 80);
        // Decoded silence should be low amplitude
        let max_amplitude = decoded.iter().map(|&s| s.abs()).max().unwrap();
        assert!(max_amplitude < 1000, "Silence decoded with amplitude {}", max_amplitude);
    }

    #[test]
    #[cfg(feature = "g729")]
    fn test_g729_encode_decode_multi_frame() {
        let mut codec = G729Codec::new().unwrap();
        // 4 frames = 320 samples
        let pcm: Vec<i16> = (0..320).map(|i| ((i as f32 * 0.1).sin() * 1000.0) as i16).collect();

        let encoded = codec.encode(&pcm).unwrap();
        // Framed format: (1 + 10) * 4 = 44 bytes
        assert_eq!(encoded.len(), 44);

        let decoded = codec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 320);
    }

    #[test]
    #[cfg(feature = "g729")]
    fn test_g729_annex_b_vad() {
        let mut codec = G729Codec::new_with_variant(G729Variant::G729B).unwrap();
        assert_eq!(codec.name(), "G.729B");

        // Encode silence - with VAD might produce SID frame (2 bytes) or untransmitted (0 bytes)
        let silence = vec![0i16; 80];
        let encoded = codec.encode(&silence).unwrap();
        // Framed format: minimum 1 byte (length prefix)
        assert!(!encoded.is_empty());

        // Should be able to decode
        let decoded = codec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 80);
    }

    #[test]
    #[cfg(feature = "g729")]
    fn test_g729_reset() {
        let mut codec = G729Codec::new().unwrap();

        // Encode some data to change internal state
        let pcm1 = vec![100i16; 80];
        let encoded1 = codec.encode(&pcm1).unwrap();

        // Reset
        codec.reset();

        // Encode same data again - should produce identical output
        let encoded2 = codec.encode(&pcm1).unwrap();
        assert_eq!(encoded1, encoded2, "Reset should restore codec to initial state");
    }

    #[test]
    #[cfg(feature = "g729")]
    fn test_g729_packet_loss_concealment() {
        let mut codec = G729Codec::new().unwrap();

        // Encode a frame
        let pcm = vec![100i16; 80];
        let encoded = codec.encode_frame_unframed(pcm.as_slice().try_into().unwrap()).unwrap();
        assert_eq!(encoded.len(), 10);

        // Decode normally
        let decoded1 = codec.decode_frame_with_plc(&encoded, false).unwrap();
        assert_eq!(decoded1.len(), 80);

        // Simulate packet loss - decode with erasure flag
        let decoded_plc = codec.decode_frame_with_plc(&[], true).unwrap();
        assert_eq!(decoded_plc.len(), 80);

        // PLC should generate reasonable output (not silence)
        let max_amplitude = decoded_plc.iter().map(|&s| s.abs()).max().unwrap();
        assert!(max_amplitude > 0, "PLC output is silent");

        // Continue decoding after packet loss
        let decoded2 = codec.decode_frame_with_plc(&encoded, false).unwrap();
        assert_eq!(decoded2.len(), 80);
    }

    #[test]
    #[cfg(feature = "g729")]
    fn test_g729_invalid_frame_size() {
        let mut codec = G729Codec::new().unwrap();
        let pcm = vec![100i16; 75]; // Wrong size

        assert!(codec.encode(&pcm).is_err());
    }

    #[test]
    #[cfg(feature = "g729")]
    fn test_g729_sine_wave_encode_decode() {
        let mut codec = G729Codec::new().unwrap();

        // Generate a 1 kHz sine wave (multiple frames for better codec adaptation)
        let sample_rate = 8000.0;
        let frequency = 1000.0;
        let amplitude = 8000.0;
        let num_frames = 4;
        let pcm: Vec<i16> = (0..(80 * num_frames))
            .map(|i| {
                let t = i as f32 / sample_rate;
                (amplitude * (2.0 * std::f32::consts::PI * frequency * t).sin()) as i16
            })
            .collect();

        let encoded = codec.encode(&pcm).unwrap();
        // Framed format: (1 + 10) * 4 = 44 bytes
        assert_eq!(encoded.len(), 44);

        let decoded = codec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 80 * num_frames);

        // G.729 is optimized for speech, not pure tones, so decoded amplitude
        // may be lower than input. Just verify it's not silence.
        let max_amplitude = decoded.iter().map(|&s| s.abs()).max().unwrap();
        assert!(max_amplitude > 10, "Decoded signal amplitude too low: {}", max_amplitude);

        // Check that there's actual signal variation (not all zeros)
        let non_zero_count = decoded.iter().filter(|&&s| s != 0).count();
        assert!(non_zero_count > decoded.len() / 2, "Decoded signal mostly zeros");
    }
}
