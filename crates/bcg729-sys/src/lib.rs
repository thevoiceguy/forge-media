//! Low-level FFI bindings to libbcg729
//!
//! This crate provides unsafe raw bindings to the bcg729 library,
//! which implements ITU-T G.729 Annex A/B speech codec.
//!
//! For safe Rust wrappers, use the `forge-codecs` crate.
//!
//! # License
//! bcg729 is licensed under GPLv3. Applications using these bindings
//! must comply with GPL requirements.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

/// Opaque encoder channel context structure
#[repr(C)]
pub struct bcg729EncoderChannelContextStruct {
    _private: [u8; 0],
}

/// Opaque decoder channel context structure
#[repr(C)]
pub struct bcg729DecoderChannelContextStruct {
    _private: [u8; 0],
}

extern "C" {
    // ==========================================================================
    // Encoder Functions
    // ==========================================================================

    /// Create and initialize an encoder channel context
    ///
    /// # Parameters
    /// - `enableVAD`: 1 to enable VAD/DTX (Annex B), 0 to disable
    ///
    /// # Returns
    /// Pointer to encoder context, or NULL on failure
    pub fn initBcg729EncoderChannel(enableVAD: u8) -> *mut bcg729EncoderChannelContextStruct;

    /// Free encoder channel context
    ///
    /// # Parameters
    /// - `encoderChannelContext`: Context to deallocate
    pub fn closeBcg729EncoderChannel(encoderChannelContext: *mut bcg729EncoderChannelContextStruct);

    /// Encode one frame (80 samples = 10ms) to G.729 bitstream
    ///
    /// # Parameters
    /// - `encoderChannelContext`: Active encoder context
    /// - `inputFrame`: 80 PCM samples (16-bit signed, 8kHz)
    /// - `bitStream`: Output buffer for encoded data (must be at least 10 bytes)
    /// - `bitStreamLength`: Output parameter for actual encoded size:
    ///   - 10 bytes: Normal speech frame
    ///   - 2 bytes: SID frame (comfort noise, if VAD enabled)
    ///   - 0 bytes: Untransmitted frame (silence, if VAD enabled)
    pub fn bcg729Encoder(
        encoderChannelContext: *mut bcg729EncoderChannelContextStruct,
        inputFrame: *const i16,
        bitStream: *mut u8,
        bitStreamLength: *mut u8,
    );

    /// Get RFC 3389 comfort noise payload
    ///
    /// # Parameters
    /// - `encoderChannelContext`: Context containing last CN frame
    /// - `payload`: Output buffer for 11 bytes of CN parameters
    pub fn bcg729GetRFC3389Payload(
        encoderChannelContext: *mut bcg729EncoderChannelContextStruct,
        payload: *mut u8,
    );

    // ==========================================================================
    // Decoder Functions
    // ==========================================================================

    /// Create and initialize a decoder channel context
    ///
    /// # Returns
    /// Pointer to decoder context, or NULL on failure
    pub fn initBcg729DecoderChannel() -> *mut bcg729DecoderChannelContextStruct;

    /// Free decoder channel context
    ///
    /// # Parameters
    /// - `decoderChannelContext`: Context to deallocate
    pub fn closeBcg729DecoderChannel(decoderChannelContext: *mut bcg729DecoderChannelContextStruct);

    /// Decode one G.729 frame to PCM samples
    ///
    /// # Parameters
    /// - `decoderChannelContext`: Active decoder context
    /// - `bitStream`: Encoded data (10 bytes for normal frame, 2 for SID, 0 for erasure)
    /// - `bitStreamLength`: Length of encoded data in bytes
    /// - `frameErasureFlag`: 1 if frame is lost/erased, 0 otherwise
    /// - `SIDFrameFlag`: 1 if frame is SID (comfort noise), 0 otherwise
    /// - `rfc3389PayloadFlag`: 1 if using RFC 3389 CN payload, 0 otherwise
    /// - `signal`: Output buffer for 80 PCM samples (16-bit signed)
    pub fn bcg729Decoder(
        decoderChannelContext: *mut bcg729DecoderChannelContextStruct,
        bitStream: *const u8,
        bitStreamLength: u8,
        frameErasureFlag: u8,
        SIDFrameFlag: u8,
        rfc3389PayloadFlag: u8,
        signal: *mut i16,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_init_close() {
        unsafe {
            let ctx = initBcg729EncoderChannel(0);
            assert!(!ctx.is_null());
            closeBcg729EncoderChannel(ctx);
        }
    }

    #[test]
    fn test_decoder_init_close() {
        unsafe {
            let ctx = initBcg729DecoderChannel();
            assert!(!ctx.is_null());
            closeBcg729DecoderChannel(ctx);
        }
    }
}
