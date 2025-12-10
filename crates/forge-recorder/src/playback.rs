//! Simple audio playback helpers for announcements/IVR.

use crate::{RecorderError, Result};
#[cfg(feature = "opus")]
use audiopus::coder::Decoder as OpusDecoder;
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
}

#[cfg(feature = "opus")]
impl OpusPlayback {
    fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let mut packets = ogg::reading::PacketReader::new(BufReader::new(file));

        // Skip OpusHead and OpusTags packets
        let _ = packets.read_packet();
        let _ = packets.read_packet();

        let decoder = OpusDecoder::new(audiopus::SampleRate::Hz48000, audiopus::Channels::Mono)
            .map_err(|e| {
                RecorderError::Encoding(format!("Failed to create Opus decoder: {:?}", e))
            })?;

        Ok(Self { decoder, packets })
    }

    fn next_samples(&mut self, _max: usize) -> Result<Option<Vec<i16>>> {
        match self.packets.read_packet()? {
            Some(packet) => {
                tracing::trace!("Decoding Opus packet: {} bytes", packet.data.len());
                let mut buf = vec![0i16; 1920]; // 40ms at 48kHz mono (48000 * 0.04)
                let len = self
                    .decoder
                    .decode(Some(&packet.data), &mut buf, false)
                    .map_err(|e| {
                        tracing::error!("Opus decode error: {:?}", e);
                        RecorderError::Encoding(format!("Opus decode failed: {:?}", e))
                    })?;
                buf.truncate(len);
                tracing::trace!("Decoded {} samples from Opus packet", len);
                Ok(Some(buf))
            }
            None => {
                tracing::trace!("No more Opus packets available");
                Ok(None)
            }
        }
    }
}
