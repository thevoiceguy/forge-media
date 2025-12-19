//! Simple audio playback helpers for announcements/IVR.

use crate::{RecorderError, Result};
#[cfg(feature = "opus")]
use audiopus::coder::Decoder as OpusDecoder;
#[cfg(feature = "opus")]
use audiopus::{Channels, SampleRate};
use hound::WavReader;
use std::io::BufReader;
use std::path::Path;

/// Stream PCM samples from an audio file for injection/announcements.
pub struct PlaybackSource {
    reader: PlaybackReader,
}

impl PlaybackSource {
    /// Open a WAV/Opus file for playback.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let ext = path_ref
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        tracing::debug!("Opening playback source: {:?} (ext: {})", path_ref, ext);

        if ext == "wav" {
            let reader = WavReader::open(path_ref)?;
            let spec = reader.spec();
            tracing::debug!(
                "WAV file opened: channels={}, sample_rate={}, bits_per_sample={}",
                spec.channels,
                spec.sample_rate,
                spec.bits_per_sample
            );
            Ok(Self {
                reader: PlaybackReader::Wav(reader),
            })
        } else if ext == "ogg" || ext == "opus" {
            #[cfg(feature = "opus")]
            {
                let reader = OpusPlayback::open(path_ref)?;
                tracing::debug!("Opus file opened for playback");
                Ok(Self {
                    reader: PlaybackReader::Opus(reader),
                })
            }
            #[cfg(not(feature = "opus"))]
            {
                tracing::warn!("Attempted to open Opus file but opus feature not enabled");
                Err(RecorderError::UnsupportedCodec(
                    "Opus playback requires the `opus` feature".into(),
                ))
            }
        } else {
            tracing::warn!("Unsupported playback file extension: {}", ext);
            Err(RecorderError::UnsupportedCodec(format!(
                "Unsupported playback extension: {}",
                ext
            )))
        }
    }

    /// Read up to `max_samples` samples. Returns `Ok(None)` on EOF.
    pub fn next_samples(&mut self, max_samples: usize) -> Result<Option<Vec<i16>>> {
        tracing::trace!("Reading up to {} samples from playback source", max_samples);

        match &mut self.reader {
            PlaybackReader::Wav(reader) => {
                let mut out = Vec::with_capacity(max_samples);
                for _ in 0..max_samples {
                    match reader.samples::<i16>().next() {
                        Some(Ok(s)) => out.push(s),
                        Some(Err(e)) => {
                            tracing::error!("Error reading WAV sample: {}", e);
                            return Err(e.into());
                        }
                        None => {
                            tracing::debug!(
                                "WAV playback EOF reached after {} samples in this read",
                                out.len()
                            );
                            break;
                        }
                    }
                }
                if out.is_empty() {
                    tracing::debug!("WAV playback completely finished (EOF)");
                    Ok(None)
                } else {
                    tracing::trace!("Read {} WAV samples", out.len());
                    Ok(Some(out))
                }
            }
            #[cfg(feature = "opus")]
            PlaybackReader::Opus(reader) => {
                let result = reader.next_samples(max_samples);
                match &result {
                    Ok(Some(samples)) => {
                        tracing::trace!("Read {} Opus samples", samples.len());
                    }
                    Ok(None) => {
                        tracing::debug!("Opus playback completely finished (EOF)");
                    }
                    Err(e) => {
                        tracing::error!("Error reading Opus samples: {}", e);
                    }
                }
                result
            }
        }
    }
}

enum PlaybackReader {
    Wav(WavReader<BufReader<std::fs::File>>),
    #[cfg(feature = "opus")]
    Opus(OpusPlayback),
}

#[cfg(feature = "opus")]
struct OpusPlayback {
    decoder: OpusDecoder,
    packets: ogg::reading::PacketReader<BufReader<std::fs::File>>,
    preskip_remaining: usize,
    pending: Vec<i16>,
}

#[cfg(feature = "opus")]
impl OpusPlayback {
    fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let mut packets = ogg::reading::PacketReader::new(BufReader::new(file));

        // Parse OpusHead
        let head_packet = packets
            .read_packet()?
            .ok_or_else(|| RecorderError::InvalidFormat("Missing OpusHead packet".into()))?;
        let (sample_rate, channels, preskip) = parse_opus_head(&head_packet.data)?;

        // Skip OpusTags
        let _ = packets.read_packet();

        let decoder = OpusDecoder::new(sample_rate, channels).map_err(|e| {
            RecorderError::Encoding(format!("Failed to create Opus decoder: {:?}", e))
        })?;

        Ok(Self {
            decoder,
            packets,
            preskip_remaining: preskip as usize,
            pending: Vec::new(),
        })
    }

    fn next_samples(&mut self, max: usize) -> Result<Option<Vec<i16>>> {
        if max == 0 {
            return Ok(Some(Vec::new()));
        }

        let mut out = Vec::with_capacity(max);

        // Drain any pending decoded samples first
        if !self.pending.is_empty() {
            let take = max.min(self.pending.len());
            out.extend_from_slice(&self.pending[..take]);
            self.pending.drain(..take);
            if out.len() == max {
                return Ok(Some(out));
            }
        }

        while out.len() < max {
            match self.packets.read_packet()? {
                Some(packet) => {
                    tracing::trace!("Decoding Opus packet: {} bytes", packet.data.len());
                    // Max Opus packet is 120ms -> 5760 samples per channel at 48kHz.
                    let mut buf = vec![0i16; 5760 * 2];
                    let len = self
                        .decoder
                        .decode(Some(&packet.data), &mut buf, false)
                        .map_err(|e| {
                            tracing::error!("Opus decode error: {:?}", e);
                            RecorderError::Encoding(format!("Opus decode failed: {:?}", e))
                        })?;
                    buf.truncate(len);

                    // Apply preskip from header
                    if self.preskip_remaining > 0 {
                        let skip = self.preskip_remaining.min(buf.len());
                        buf.drain(..skip);
                        self.preskip_remaining -= skip;
                    }

                    if buf.is_empty() {
                        continue;
                    }

                    let remaining_capacity = max - out.len();
                    if buf.len() <= remaining_capacity {
                        tracing::trace!("Decoded {} samples from Opus packet", buf.len());
                        out.extend_from_slice(&buf);
                    } else {
                        out.extend_from_slice(&buf[..remaining_capacity]);
                        self.pending.extend_from_slice(&buf[remaining_capacity..]);
                        tracing::trace!(
                            "Decoded {} samples, delivering {} and buffering {}",
                            buf.len(),
                            remaining_capacity,
                            buf.len() - remaining_capacity
                        );
                    }
                }
                None => break,
            }
        }

        if out.is_empty() {
            tracing::trace!("No more Opus packets available");
            Ok(None)
        } else {
            Ok(Some(out))
        }
    }
}

#[cfg(feature = "opus")]
fn parse_opus_head(data: &[u8]) -> Result<(SampleRate, Channels, u16)> {
    if data.len() < 19 || &data[..8] != b"OpusHead" {
        return Err(RecorderError::InvalidFormat(
            "Invalid OpusHead packet".into(),
        ));
    }

    let channels = data[9];
    let preskip = u16::from_le_bytes([data[10], data[11]]);
    let input_sample_rate = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    let channel_mapping = data[18];

    let sample_rate = match input_sample_rate {
        8000 => SampleRate::Hz8000,
        12000 => SampleRate::Hz12000,
        16000 => SampleRate::Hz16000,
        24000 => SampleRate::Hz24000,
        48000 | 0 => SampleRate::Hz48000,
        other => {
            tracing::warn!("Unsupported Opus sample rate {} in header, using 48kHz", other);
            SampleRate::Hz48000
        }
    };

    let channels = match (channels, channel_mapping) {
        (1, 0) => Channels::Mono,
        (2, 0) => Channels::Stereo,
        _ => {
            return Err(RecorderError::UnsupportedCodec(format!(
                "Unsupported Opus channel layout (channels={}, mapping={})",
                channels, channel_mapping
            )))
        }
    };

    Ok((sample_rate, channels, preskip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioFormat, AudioRecorder};
    use tempfile::TempDir;

    #[cfg(feature = "opus")]
    #[test]
    fn test_opus_playback_respects_max_samples() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.opus");

        // Build a small Opus recording to read back
        {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let format = AudioFormat::opus_mono();
                let recorder = AudioRecorder::new(&file_path, format).await.unwrap();
                recorder.start().unwrap();
                let samples = vec![5i16; 960 * 4]; // 4 frames
                recorder.write_samples(&samples).unwrap();
                recorder.stop().unwrap();
            });
        }

        let mut playback = PlaybackSource::open(&file_path).unwrap();

        let first = playback.next_samples(1000).unwrap().unwrap();
        assert!(first.len() <= 1000);

        let second = playback.next_samples(1000).unwrap().unwrap();
        assert!(second.len() <= 1000);

        let third = playback.next_samples(1000).unwrap().unwrap();
        assert!(third.len() <= 1000);

        let end = playback.next_samples(1000).unwrap();
        assert!(end.is_none());
    }
}
