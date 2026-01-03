//! RTP packet transcoding helpers
//!
//! Provides utilities for transcoding RTP audio packets, handling
//! frame buffering, timestamp adjustments, and codec detection.

use crate::{AudioFormat, Result, Transcoder};
use forge_codecs::AudioCodecType;
use std::collections::VecDeque;

/// RTP payload type mapping
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadTypeMap {
    /// PCMU (G.711 µ-law) payload type (typically 0)
    pub pcmu: u8,
    /// PCMA (G.711 A-law) payload type (typically 8)
    pub pcma: u8,
    /// Opus payload type (dynamic, typically 111)
    pub opus: u8,
}

impl Default for PayloadTypeMap {
    fn default() -> Self {
        Self {
            pcmu: 0,
            pcma: 8,
            opus: 111,
        }
    }
}

impl PayloadTypeMap {
    /// Convert RTP payload type to codec type
    pub fn to_codec(&self, payload_type: u8) -> Option<AudioCodecType> {
        match payload_type {
            pt if pt == self.pcmu => Some(AudioCodecType::PCMU),
            pt if pt == self.pcma => Some(AudioCodecType::PCMA),
            pt if pt == self.opus => Some(AudioCodecType::Opus),
            9 => Some(AudioCodecType::G722),
            18 => Some(AudioCodecType::G729),
            _ => None,
        }
    }

    /// Convert codec type to RTP payload type
    pub fn from_codec(&self, codec: AudioCodecType) -> Option<u8> {
        match codec {
            AudioCodecType::PCMU => Some(self.pcmu),
            AudioCodecType::PCMA => Some(self.pcma),
            AudioCodecType::Opus => Some(self.opus),
            AudioCodecType::G722 => Some(9), // RFC 3551 static payload type
            AudioCodecType::G729 => Some(18), // RFC 3551 static payload type
            AudioCodecType::PCM => None,
        }
    }
}

/// RTP transcoder with packet buffering and timestamp management
///
/// Handles transcoding of RTP packets between different codecs, managing
/// frame size differences and RTP metadata (timestamps, sequence numbers).
pub struct RtpTranscoder {
    /// Underlying transcoder
    transcoder: Transcoder,
    /// Payload type mapping
    pt_map: PayloadTypeMap,
    /// Last input timestamp
    last_input_ts: Option<u32>,
    /// Output timestamp offset
    output_ts_offset: u32,
    /// Packet buffer for output
    output_buffer: VecDeque<Vec<u8>>,
}

impl RtpTranscoder {
    /// Create a new RTP transcoder
    ///
    /// # Arguments
    ///
    /// * `src_codec` - Source codec type
    /// * `dst_codec` - Destination codec type
    /// * `pt_map` - Payload type mapping
    pub fn new(
        src_codec: AudioCodecType,
        dst_codec: AudioCodecType,
        pt_map: PayloadTypeMap,
    ) -> Result<Self> {
        let src_format = Self::codec_to_format(src_codec);
        let dst_format = Self::codec_to_format(dst_codec);

        let transcoder = Transcoder::new(src_format, dst_format)?;

        Ok(Self {
            transcoder,
            pt_map,
            last_input_ts: None,
            output_ts_offset: 0,
            output_buffer: VecDeque::new(),
        })
    }

    /// Convert codec type to audio format with default parameters
    fn codec_to_format(codec: AudioCodecType) -> AudioFormat {
        let (sample_rate, channels) = match codec {
            AudioCodecType::PCMU | AudioCodecType::PCMA => (8000, 1),
            AudioCodecType::G722 => (16000, 1), // Wideband
            AudioCodecType::G729 => (8000, 1),  // Narrowband
            AudioCodecType::Opus => (48000, 1),
            AudioCodecType::PCM => (8000, 1),
        };

        AudioFormat {
            sample_rate,
            channels,
            codec,
        }
    }

    /// Transcode an RTP packet
    ///
    /// # Returns
    ///
    /// Vector of transcoded payloads. May be empty if buffering is needed,
    /// or contain multiple payloads if frame size adaptation produces multiple frames.
    pub fn transcode_rtp_payload(&mut self, payload: &[u8]) -> Result<Vec<Vec<u8>>> {
        let result = self.transcoder.transcode_frames(payload)?;
        for frame in result.frames {
            self.output_buffer.push_back(frame);
        }
        Ok(self.output_buffer.drain(..).collect())
    }

    /// Transcode an RTP payload and return output payloads with timestamps
    pub fn transcode_rtp_payload_with_timestamp(
        &mut self,
        payload: &[u8],
        input_ts: u32,
    ) -> Result<Vec<(u32, Vec<u8>)>> {
        let src_rate = self.transcoder.src_format().sample_rate as u64;
        let dst_rate = self.transcoder.dst_format().sample_rate as u64;
        let scaled_ts = ((input_ts as u64) * dst_rate / src_rate) as u32;

        if self
            .last_input_ts
            .map(|last| last != input_ts)
            .unwrap_or(true)
        {
            self.output_ts_offset = scaled_ts;
        }
        self.last_input_ts = Some(input_ts);

        let result = self.transcoder.transcode_frames(payload)?;
        let frame_samples = result.frame_samples.unwrap_or(result.samples_per_channel);

        let mut outputs = Vec::new();
        let mut current_ts = self.output_ts_offset;
        for frame in result.frames {
            outputs.push((current_ts, frame));
            current_ts = current_ts.wrapping_add(frame_samples as u32);
        }

        self.output_ts_offset = current_ts;
        Ok(outputs)
    }

    /// Get source codec type
    pub fn src_codec(&self) -> AudioCodecType {
        self.transcoder.src_format().codec
    }

    /// Get destination codec type
    pub fn dst_codec(&self) -> AudioCodecType {
        self.transcoder.dst_format().codec
    }

    /// Get payload type mapping
    pub fn pt_map(&self) -> &PayloadTypeMap {
        &self.pt_map
    }

    /// Reset transcoder state
    pub fn reset(&mut self) {
        self.transcoder.reset();
        self.last_input_ts = None;
        self.output_ts_offset = 0;
        self.output_buffer.clear();
    }

    /// Check if transcoding is actually needed
    pub fn needs_transcoding(&self) -> bool {
        self.transcoder.needs_transcoding()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_payload_type_map_default() {
        let pt_map = PayloadTypeMap::default();
        assert_eq!(pt_map.to_codec(0), Some(AudioCodecType::PCMU));
        assert_eq!(pt_map.to_codec(8), Some(AudioCodecType::PCMA));
        assert_eq!(pt_map.to_codec(111), Some(AudioCodecType::Opus));
        assert_eq!(pt_map.to_codec(9), Some(AudioCodecType::G722));
        assert_eq!(pt_map.to_codec(18), Some(AudioCodecType::G729));
        assert_eq!(pt_map.to_codec(99), None);
    }

    #[test]
    fn test_payload_type_map_from_codec() {
        let pt_map = PayloadTypeMap::default();
        assert_eq!(pt_map.from_codec(AudioCodecType::PCMU), Some(0));
        assert_eq!(pt_map.from_codec(AudioCodecType::PCMA), Some(8));
        assert_eq!(pt_map.from_codec(AudioCodecType::Opus), Some(111));
        assert_eq!(pt_map.from_codec(AudioCodecType::G722), Some(9));
        assert_eq!(pt_map.from_codec(AudioCodecType::G729), Some(18));
        assert_eq!(pt_map.from_codec(AudioCodecType::PCM), None);
    }

    #[test]
    fn test_rtp_transcoder_create() {
        let pt_map = PayloadTypeMap::default();
        let transcoder = RtpTranscoder::new(AudioCodecType::PCMU, AudioCodecType::PCMA, pt_map);
        assert!(transcoder.is_ok());

        let transcoder = transcoder.unwrap();
        assert_eq!(transcoder.src_codec(), AudioCodecType::PCMU);
        assert_eq!(transcoder.dst_codec(), AudioCodecType::PCMA);
    }

    #[test]
    fn test_rtp_transcoder_needs_transcoding() {
        let pt_map = PayloadTypeMap::default();

        let transcoder =
            RtpTranscoder::new(AudioCodecType::PCMU, AudioCodecType::PCMA, pt_map).unwrap();
        assert!(transcoder.needs_transcoding());

        let transcoder =
            RtpTranscoder::new(AudioCodecType::PCMU, AudioCodecType::PCMU, pt_map).unwrap();
        assert!(!transcoder.needs_transcoding());
    }

    #[test]
    fn test_codec_to_format() {
        let format = RtpTranscoder::codec_to_format(AudioCodecType::PCMU);
        assert_eq!(format.sample_rate, 8000);
        assert_eq!(format.channels, 1);
        assert_eq!(format.codec, AudioCodecType::PCMU);

        let format = RtpTranscoder::codec_to_format(AudioCodecType::Opus);
        assert_eq!(format.sample_rate, 48000);
        assert_eq!(format.channels, 1);
        assert_eq!(format.codec, AudioCodecType::Opus);
    }
}
