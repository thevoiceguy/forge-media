//! AAC-LC encoding over the Fraunhofer FDK AAC library, built from source
//! by `fdk-aac-sys` (feature `aac`).
//!
//! What RTMP and the services behind it take: MPEG-4 AAC Low Complexity,
//! raw access units with their `AudioSpecificConfig` sent once beside them.
//! Each access unit codes 1024 samples per channel.
//!
//! **Licensing.** The FDK library's licence permits redistribution in
//! source and binary form but grants no patent rights; AAC's patent
//! licensing is the deployer's matter, as H.264's is with OpenH264 built
//! from source. Nothing here is compiled unless the `aac` feature asks.

use crate::{CodecError, Result};

/// Samples per channel each access unit codes.
pub const AAC_FRAME: usize = 1024;

/// How to encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    /// 8 000 to 96 000 Hz; 48 000 for RTMP.
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: u16,
    /// Constant bit rate, bits per second.
    pub bitrate: u32,
}

impl Default for AacConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 1,
            bitrate: 128_000,
        }
    }
}

/// The `AudioSpecificConfig` for AAC-LC at `sample_rate` with `channels`
/// (ISO/IEC 14496-3 §1.6.2.1): what FLV's AAC sequence header and MP4's
/// `esds` carry. The encoder gives the same; this one needs no encoder.
pub fn audio_specific_config(sample_rate: u32, channels: u16) -> Option<[u8; 2]> {
    const RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
    let index = RATES.iter().position(|r| *r == sample_rate)? as u8;
    if !(1..=7).contains(&channels) {
        return None;
    }
    // 5 bits object type (2: LC), 4 bits rate index, 4 bits channels, then
    // three zero bits (frame length 1024, no core coder, no extension).
    let aot = 2u8;
    Some([
        (aot << 3) | (index >> 1),
        ((index & 1) << 7) | ((channels as u8) << 3),
    ])
}

#[cfg(feature = "aac")]
pub use imp::AacEncoder;

#[cfg(feature = "aac")]
mod imp {
    use super::*;
    use fdk_aac_sys as sys;
    use std::os::raw::{c_int, c_void};

    fn check(what: &str, e: sys::AACENC_ERROR) -> Result<()> {
        if e == sys::AACENC_ERROR_AACENC_OK {
            Ok(())
        } else {
            Err(CodecError::Encoding(format!("AAC {what}: error {e:#x}")))
        }
    }

    /// An AAC-LC encoder. Samples go in as they come; each whole
    /// 1024-sample frame comes out as one access unit.
    pub struct AacEncoder {
        handle: sys::HANDLE_AACENCODER,
        config: AacConfig,
        asc: Vec<u8>,
        pending: Vec<i16>,
        out: Vec<u8>,
    }

    // The handle is the library's own state, used from one thread at a
    // time through `&mut self`.
    unsafe impl Send for AacEncoder {}
    unsafe impl Sync for AacEncoder {}

    impl std::fmt::Debug for AacEncoder {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AacEncoder")
                .field("config", &self.config)
                .finish_non_exhaustive()
        }
    }

    impl Drop for AacEncoder {
        fn drop(&mut self) {
            unsafe {
                sys::aacEncClose(&mut self.handle);
            }
        }
    }

    impl AacEncoder {
        pub fn new(config: AacConfig) -> Result<Self> {
            if audio_specific_config(config.sample_rate, config.channels).is_none()
                || config.channels > 2
            {
                return Err(CodecError::InvalidFormat(format!(
                    "AAC: {} Hz with {} channels",
                    config.sample_rate, config.channels
                )));
            }
            let mut handle: sys::HANDLE_AACENCODER = std::ptr::null_mut();
            check("open", unsafe {
                sys::aacEncOpen(&mut handle, 0, config.channels as u32)
            })?;
            let mut enc = Self {
                handle,
                config,
                asc: Vec::new(),
                pending: Vec::with_capacity(AAC_FRAME * config.channels as usize * 2),
                out: Vec::new(),
            };
            let set = |param: sys::AACENC_PARAM, value: u32, what: &str| {
                check(what, unsafe {
                    sys::aacEncoder_SetParam(enc.handle, param, value)
                })
            };
            set(
                sys::AACENC_PARAM_AACENC_AOT,
                sys::AUDIO_OBJECT_TYPE_AOT_AAC_LC as u32,
                "object type",
            )?;
            set(
                sys::AACENC_PARAM_AACENC_SAMPLERATE,
                config.sample_rate,
                "sample rate",
            )?;
            set(
                sys::AACENC_PARAM_AACENC_CHANNELMODE,
                config.channels as u32,
                "channels",
            )?;
            // WAV channel order: interleaved left, right.
            set(sys::AACENC_PARAM_AACENC_CHANNELORDER, 1, "channel order")?;
            set(sys::AACENC_PARAM_AACENC_BITRATEMODE, 0, "bit rate mode")?;
            set(sys::AACENC_PARAM_AACENC_BITRATE, config.bitrate, "bit rate")?;
            // Raw access units: the container carries the configuration.
            set(sys::AACENC_PARAM_AACENC_TRANSMUX, 0, "transport")?;
            // The better quantiser, for a little more CPU.
            set(sys::AACENC_PARAM_AACENC_AFTERBURNER, 1, "afterburner")?;
            check("init", unsafe {
                sys::aacEncEncode(
                    enc.handle,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                )
            })?;
            let mut info: sys::AACENC_InfoStruct = unsafe { std::mem::zeroed() };
            check("info", unsafe { sys::aacEncInfo(enc.handle, &mut info) })?;
            enc.asc = info.confBuf[..info.confSize as usize].to_vec();
            enc.out = vec![0u8; (info.maxOutBufBytes as usize).max(8192)];
            Ok(enc)
        }

        pub fn config(&self) -> AacConfig {
            self.config
        }

        /// The `AudioSpecificConfig` the access units need beside them.
        pub fn audio_specific_config(&self) -> &[u8] {
            &self.asc
        }

        /// Samples the encoder holds back before its first access unit
        /// covers them: what a player shifts the audio by.
        pub fn delay(&self) -> u32 {
            let mut info: sys::AACENC_InfoStruct = unsafe { std::mem::zeroed() };
            if unsafe { sys::aacEncInfo(self.handle, &mut info) } != sys::AACENC_ERROR_AACENC_OK {
                return 0;
            }
            info.nDelay
        }

        /// Take interleaved `samples`; the access units they complete, in
        /// order. The first frames may give none while the encoder fills
        /// its look-ahead.
        pub fn encode(&mut self, samples: &[i16]) -> Result<Vec<Vec<u8>>> {
            self.pending.extend_from_slice(samples);
            let frame = AAC_FRAME * self.config.channels as usize;
            let mut units = Vec::new();
            while self.pending.len() >= frame {
                let chunk: Vec<i16> = self.pending.drain(..frame).collect();
                if let Some(unit) = self.encode_call(Some(&chunk))? {
                    units.push(unit);
                }
            }
            Ok(units)
        }

        /// The end: what the encoder still holds, flushed out, the last
        /// partial frame padded with silence.
        pub fn flush(&mut self) -> Result<Vec<Vec<u8>>> {
            let mut units = Vec::new();
            if !self.pending.is_empty() {
                let frame = AAC_FRAME * self.config.channels as usize;
                self.pending.resize(frame, 0);
                let chunk = std::mem::take(&mut self.pending);
                if let Some(unit) = self.encode_call(Some(&chunk))? {
                    units.push(unit);
                }
            }
            // A bounded drain: the library ends with ENCODE_EOF.
            for _ in 0..8 {
                match self.encode_call(None) {
                    Ok(Some(unit)) => units.push(unit),
                    Ok(None) | Err(_) => break,
                }
            }
            Ok(units)
        }

        fn encode_call(&mut self, input: Option<&[i16]>) -> Result<Option<Vec<u8>>> {
            let (mut in_ptr, in_len) = match input {
                Some(s) => (s.as_ptr() as *mut c_void, s.len() as c_int),
                None => (std::ptr::null_mut(), 0),
            };
            let mut in_id: c_int = sys::AACENC_BufferIdentifier_IN_AUDIO_DATA as c_int;
            let mut in_size: c_int = in_len * 2;
            let mut in_el: c_int = 2;
            let in_desc = sys::AACENC_BufDesc {
                numBufs: 1,
                bufs: &mut in_ptr,
                bufferIdentifiers: &mut in_id,
                bufSizes: &mut in_size,
                bufElSizes: &mut in_el,
            };
            let mut out_ptr = self.out.as_mut_ptr() as *mut c_void;
            let mut out_id: c_int = sys::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as c_int;
            let mut out_size: c_int = self.out.len() as c_int;
            let mut out_el: c_int = 1;
            let out_desc = sys::AACENC_BufDesc {
                numBufs: 1,
                bufs: &mut out_ptr,
                bufferIdentifiers: &mut out_id,
                bufSizes: &mut out_size,
                bufElSizes: &mut out_el,
            };
            let in_args = sys::AACENC_InArgs {
                // -1 asks the library to flush.
                numInSamples: if input.is_some() { in_len } else { -1 },
                numAncBytes: 0,
            };
            let mut out_args: sys::AACENC_OutArgs = unsafe { std::mem::zeroed() };
            let e = unsafe {
                sys::aacEncEncode(self.handle, &in_desc, &out_desc, &in_args, &mut out_args)
            };
            if e == sys::AACENC_ERROR_AACENC_ENCODE_EOF {
                return Ok(None);
            }
            check("encode", e)?;
            let n = out_args.numOutBytes as usize;
            Ok((n > 0).then(|| self.out[..n].to_vec()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audio_specific_config_is_aac_lc_at_its_rate_and_channels() {
        // 48 kHz mono: object type 2, index 3, one channel.
        assert_eq!(audio_specific_config(48_000, 1), Some([0x11, 0x88]));
        // 44.1 kHz stereo: index 4, two channels.
        assert_eq!(audio_specific_config(44_100, 2), Some([0x12, 0x10]));
        assert_eq!(audio_specific_config(47_000, 1), None);
        assert_eq!(audio_specific_config(48_000, 0), None);
    }

    #[cfg(feature = "aac")]
    #[test]
    fn a_tone_encodes_and_decodes_back_to_itself() {
        use fdk_aac::dec::{Decoder, DecoderError, Transport};
        let mut enc = AacEncoder::new(AacConfig::default()).unwrap();
        assert_eq!(
            enc.audio_specific_config(),
            &audio_specific_config(48_000, 1).unwrap()[..],
            "the library's configuration is the one FLV is sent"
        );
        // Two seconds of 1 kHz.
        let tone: Vec<i16> = (0..96_000)
            .map(|i| {
                ((i as f64 * 2.0 * std::f64::consts::PI * 1000.0 / 48_000.0).sin() * 12_000.0)
                    as i16
            })
            .collect();
        let mut units = Vec::new();
        for chunk in tone.chunks(960) {
            units.extend(enc.encode(chunk).unwrap());
        }
        units.extend(enc.flush().unwrap());
        assert!(units.len() >= 90, "about 94 frames: {}", units.len());
        let bytes: usize = units.iter().map(Vec::len).sum();
        // 128 kb/s over two seconds is 32 000 bytes.
        assert!(
            (24_000..40_000).contains(&bytes),
            "about 128 kb/s: {bytes} bytes"
        );

        let mut dec = Decoder::new(Transport::Raw);
        dec.config_raw(enc.audio_specific_config()).unwrap();
        let mut pcm = Vec::new();
        let mut frame = vec![0i16; 2048 * 2];
        for unit in &units {
            dec.fill(unit).unwrap();
            match dec.decode_frame(&mut frame) {
                Ok(()) => pcm.extend_from_slice(&frame[..dec.decoded_frame_size()]),
                Err(e) if e == DecoderError::NOT_ENOUGH_BITS => {}
                Err(e) => panic!("decoding: {e}"),
            }
        }
        // Past the encoder's delay, the decoded audio is the tone: its
        // energy at 1 kHz dwarfs its energy at 3 kHz.
        let window = &pcm[24_000..48_000];
        let power = |f: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, s) in window.iter().enumerate() {
                let w = i as f64 * 2.0 * std::f64::consts::PI * f / 48_000.0;
                re += *s as f64 * w.cos();
                im += *s as f64 * w.sin();
            }
            re * re + im * im
        };
        assert!(
            power(1000.0) > 1000.0 * power(3000.0),
            "a 1 kHz tone comes back as one"
        );
        let peak = window.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(
            (10_000..14_000).contains(&peak),
            "at its level: peak {peak}"
        );
    }
}
