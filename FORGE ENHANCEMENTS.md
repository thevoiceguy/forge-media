# Forge Media Engine - Enhancement Recommendations

**Version:** 1.0
**Date:** December 2024
**Status:** Design Review & Recommendations

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Current State Assessment](#current-state-assessment)
3. [Missing Carrier-Grade Features](#missing-carrier-grade-features)
4. [FCP Integration Improvements](#fcp-integration-improvements)
5. [Deployment & Operations](#deployment--operations)
6. [Priority Matrix](#priority-matrix)
7. [Implementation Roadmap](#implementation-roadmap)

---

## Executive Summary

The Forge media engine design is comprehensive and well-architected. This document identifies additional features required to make Forge truly carrier/enterprise-grade, along with recommendations for seamless FCP integration while maintaining Forge's standalone usability.

### Key Findings

**Strengths:**
- ✅ Solid RTP/RTCP/SRTP foundation
- ✅ Comprehensive audio codec support
- ✅ Well-designed conferencing with VAD/AGC
- ✅ SIPREC implementation (RFC 7865/7866)
- ✅ AI streaming integration
- ✅ High availability architecture

**Gaps Identified:**
- ❌ No video support (critical for modern platforms)
- ❌ Limited audio DSP (no AEC/noise reduction)
- ❌ Basic observability (needs Prometheus/OpenTelemetry)
- ❌ Incomplete multi-tenancy model
- ❌ Missing compliance features (lawful intercept, E911)
- ❌ Type system not aligned with FCP core

---

## Current State Assessment

### What's Already Designed

| Category | Coverage | Notes |
|----------|----------|-------|
| **Audio Transport** | ✅ Complete | RTP/RTCP, SRTP, jitter buffer, DTLS-SRTP |
| **Audio Codecs** | ✅ Complete | G.711, G.722, G.729, Opus, Speex, iLBC, AMR |
| **Audio Conferencing** | ✅ Complete | Mixing, VAD, AGC, dominant speaker |
| **Recording** | ✅ Complete | Multi-format, storage backends, SIPREC |
| **DTMF** | ✅ Complete | RFC 2833, in-band detection |
| **Transcription** | ✅ Complete | Multiple STT providers |
| **Audio Injection** | ✅ Complete | File, TTS, tone generation |
| **WebRTC** | ⚠️ Partial | ICE, DTLS-SRTP (audio only) |
| **AI Streaming** | ✅ Complete | OpenAI, Dialogflow, Lex, Azure |
| **High Availability** | ✅ Complete | Session replication, VIP failover |
| **Video Support** | ❌ Missing | No video codecs or conferencing |
| **Audio DSP** | ❌ Missing | No AEC, noise reduction, or advanced processing |
| **Observability** | ⚠️ Basic | Needs Prometheus/OpenTelemetry integration |
| **Multi-tenancy** | ⚠️ Basic | Needs quotas, isolation, per-tenant limits |
| **Compliance** | ❌ Missing | No lawful intercept, E911, STIR/SHAKEN |
| **QoE Monitoring** | ⚠️ Basic | Needs MOS calculation, quality alerts |

---

## Missing Carrier-Grade Features

### 1. Video Support (Critical Gap)

**Priority:** 🔴 High
**Impact:** Without video, Forge cannot support modern UC platforms.

#### Video Codecs

```rust
// crates/forge-video/src/codecs/mod.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264/AVC - Most widely supported
    H264 {
        profile: H264Profile,
        level: u8,
        packetization_mode: u8,
    },
    /// H.265/HEVC - Better compression
    H265 {
        profile: H265Profile,
    },
    /// VP8 - Open, widely used in WebRTC
    VP8,
    /// VP9 - Successor to VP8
    VP9 {
        profile: u8,
    },
    /// AV1 - Next-gen open codec
    AV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264Profile {
    Baseline,
    Main,
    High,
    ConstrainedBaseline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H265Profile {
    Main,
    Main10,
    MainStillPicture,
}

/// Video format descriptor
#[derive(Debug, Clone)]
pub struct VideoFormat {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub bitrate: u32,
}

impl VideoFormat {
    pub fn hd_720p() -> Self {
        Self {
            codec: VideoCodec::H264 {
                profile: H264Profile::Main,
                level: 31,
                packetization_mode: 1,
            },
            width: 1280,
            height: 720,
            framerate: 30,
            bitrate: 2_000_000,
        }
    }

    pub fn full_hd_1080p() -> Self {
        Self {
            codec: VideoCodec::H264 {
                profile: H264Profile::High,
                level: 40,
                packetization_mode: 1,
            },
            width: 1920,
            height: 1080,
            framerate: 30,
            bitrate: 4_000_000,
        }
    }
}
```

#### Video Transcoding

```rust
// crates/forge-video/src/transcoder.rs

use ffmpeg_next as ffmpeg;

pub struct VideoTranscoder {
    decoder: Box<dyn VideoDecoder>,
    encoder: Box<dyn VideoEncoder>,
    scaler: VideoScaler,
    filters: Vec<Box<dyn VideoFilter>>,
}

pub trait VideoDecoder: Send + Sync {
    fn decode(&mut self, packet: &[u8]) -> Result<VideoFrame, CodecError>;
    fn codec(&self) -> VideoCodec;
    fn resolution(&self) -> (u32, u32);
}

pub trait VideoEncoder: Send + Sync {
    fn encode(&mut self, frame: &VideoFrame) -> Result<Vec<u8>, CodecError>;
    fn codec(&self) -> VideoCodec;
    fn set_bitrate(&mut self, bitrate: u32);
    fn request_keyframe(&mut self);
}

pub struct VideoFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub timestamp: u64,
    pub is_keyframe: bool,
}

pub enum PixelFormat {
    Yuv420p,
    Nv12,
    Rgb24,
}

pub struct VideoScaler {
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    algorithm: ScaleAlgorithm,
}

pub enum ScaleAlgorithm {
    FastBilinear,
    Bilinear,
    Bicubic,
    Lanczos,
}

pub trait VideoFilter: Send + Sync {
    fn apply(&mut self, frame: &mut VideoFrame) -> Result<(), FilterError>;
}

/// Common video filters
pub struct RotationFilter {
    pub angle: u32, // 0, 90, 180, 270
}

pub struct WatermarkFilter {
    pub image: Vec<u8>,
    pub x: u32,
    pub y: u32,
    pub opacity: f32,
}

pub struct PictureInPictureFilter {
    pub secondary_stream: VideoStream,
    pub position: PipPosition,
    pub scale: f32,
}

pub enum PipPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}
```

#### Video Conferencing

```rust
// crates/forge-video/src/conference.rs

pub struct VideoConference {
    room: Arc<ConferenceRoom>,
    participants: DashMap<ParticipantId, VideoParticipant>,
    layout: VideoLayout,
    compositor: VideoCompositor,
}

pub enum VideoLayout {
    /// Grid layout (e.g., 2x2, 3x3)
    Grid {
        columns: usize,
        rows: usize,
    },
    /// Active speaker takes full screen
    ActiveSpeaker,
    /// Active speaker large, others small
    PictureInPicture {
        pip_count: usize,
        pip_position: PipPosition,
    },
    /// Side-by-side split
    SideBySide,
    /// Presentation mode (shared screen + thumbnails)
    Presentation {
        screen_ratio: f32,
    },
    /// Custom layout definition
    Custom(LayoutDefinition),
}

pub struct LayoutDefinition {
    pub width: u32,
    pub height: u32,
    pub regions: Vec<LayoutRegion>,
}

pub struct LayoutRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub participant_id: Option<ParticipantId>,
    pub role: RegionRole,
}

pub enum RegionRole {
    ActiveSpeaker,
    DominantSpeaker,
    FixedParticipant(ParticipantId),
    ScreenShare,
}

pub struct VideoCompositor {
    layout: VideoLayout,
    output_format: VideoFormat,
    canvas: VideoCanvas,
}

impl VideoCompositor {
    /// Compose multiple video streams into a single output
    pub fn compose(&mut self, streams: &[VideoStream]) -> Result<VideoFrame> {
        let mut canvas = self.canvas.clear();

        match &self.layout {
            VideoLayout::Grid { columns, rows } => {
                self.compose_grid(&mut canvas, streams, *columns, *rows)?;
            }
            VideoLayout::ActiveSpeaker => {
                self.compose_active_speaker(&mut canvas, streams)?;
            }
            VideoLayout::PictureInPicture { pip_count, pip_position } => {
                self.compose_pip(&mut canvas, streams, *pip_count, *pip_position)?;
            }
            _ => {}
        }

        Ok(canvas.to_frame())
    }

    fn compose_grid(
        &self,
        canvas: &mut VideoCanvas,
        streams: &[VideoStream],
        columns: usize,
        rows: usize,
    ) -> Result<()> {
        let cell_width = canvas.width / columns as u32;
        let cell_height = canvas.height / rows as u32;

        for (i, stream) in streams.iter().enumerate().take(columns * rows) {
            let col = i % columns;
            let row = i / columns;
            let x = col as u32 * cell_width;
            let y = row as u32 * cell_height;

            canvas.draw_stream(stream, x, y, cell_width, cell_height)?;
        }

        Ok(())
    }
}
```

#### Video Recording

```rust
// crates/forge-recording/src/video.rs

pub struct VideoRecorder {
    encoder: VideoEncoder,
    muxer: Muxer,
    format: VideoRecordingFormat,
}

pub enum VideoRecordingFormat {
    /// MP4 container with H.264
    Mp4H264,
    /// WebM container with VP8/VP9
    WebM { codec: WebMCodec },
    /// MKV container (flexible)
    Mkv { video_codec: VideoCodec, audio_codec: AudioCodec },
}

pub enum WebMCodec {
    VP8,
    VP9,
}

impl VideoRecorder {
    pub async fn record_frame(&mut self, frame: VideoFrame) -> Result<()> {
        let encoded = self.encoder.encode(&frame)?;
        self.muxer.write_packet(encoded)?;
        Ok(())
    }
}
```

**FCP Integration:**
- Video adds complexity to FCP B2BUA scenarios - ensure smooth SDP negotiation for mixed audio/video calls
- Store video codec preferences in tenant configuration
- Emit video quality events to FCP event bus

---

### 2. Advanced RTCP Feedback & Congestion Control

**Priority:** 🔴 High
**Impact:** Essential for video quality and bandwidth adaptation.

```rust
// crates/forge-rtp/src/rtcp_feedback.rs

use std::time::Instant;

/// RTCP feedback messages for quality control
#[derive(Debug, Clone)]
pub enum RtcpFeedback {
    /// Negative Acknowledgment - request retransmission
    NACK {
        ssrc: u32,
        lost_packets: Vec<u16>,
    },
    /// Full Intra Request - request keyframe
    FIR {
        ssrc: u32,
        sequence: u8,
    },
    /// Picture Loss Indication - request keyframe
    PLI {
        ssrc: u32,
    },
    /// Receiver Estimated Maximum Bitrate
    REMB {
        ssrc: u32,
        bitrate_bps: u64,
    },
    /// Temporary Maximum Media Bitrate Request
    TMMBR {
        ssrc: u32,
        bitrate_bps: u64,
        overhead: u16,
    },
    /// Temporal-Spatial Trade-off Request
    TSTR {
        ssrc: u32,
        request_id: u32,
    },
}

pub struct RtcpFeedbackHandler {
    sender: Arc<RtpSender>,
    receiver: Arc<RtpReceiver>,
}

impl RtcpFeedbackHandler {
    pub async fn handle_feedback(&mut self, feedback: RtcpFeedback) -> Result<()> {
        match feedback {
            RtcpFeedback::NACK { ssrc, lost_packets } => {
                self.handle_nack(ssrc, lost_packets).await?;
            }
            RtcpFeedback::FIR { ssrc, sequence } => {
                self.handle_fir(ssrc, sequence).await?;
            }
            RtcpFeedback::PLI { ssrc } => {
                self.handle_pli(ssrc).await?;
            }
            RtcpFeedback::REMB { ssrc, bitrate_bps } => {
                self.handle_remb(ssrc, bitrate_bps).await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_nack(&self, ssrc: u32, lost_packets: Vec<u16>) -> Result<()> {
        // Retransmit lost packets from sender buffer
        for seq in lost_packets {
            if let Some(packet) = self.sender.get_buffered_packet(ssrc, seq) {
                self.sender.retransmit(packet).await?;
            }
        }
        Ok(())
    }

    async fn handle_fir(&self, ssrc: u32, sequence: u8) -> Result<()> {
        // Request encoder to generate keyframe
        self.sender.request_keyframe(ssrc).await?;
        Ok(())
    }

    async fn handle_remb(&self, ssrc: u32, bitrate_bps: u64) -> Result<()> {
        // Adjust encoder bitrate based on receiver feedback
        self.sender.set_target_bitrate(ssrc, bitrate_bps).await?;
        Ok(())
    }
}
```

#### Congestion Control

```rust
// crates/forge-rtp/src/congestion.rs

pub struct CongestionController {
    algorithm: Box<dyn CongestionAlgorithm>,
    target_bitrate: u64,
    current_bitrate: u64,
    rtt_estimator: RttEstimator,
    loss_detector: LossDetector,
    state: CongestionState,
}

pub trait CongestionAlgorithm: Send + Sync {
    fn update(
        &mut self,
        rtt: Duration,
        loss_rate: f32,
        received_bitrate: u64,
    ) -> BitrateUpdate;
}

pub struct BitrateUpdate {
    pub new_bitrate: u64,
    pub state: CongestionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CongestionState {
    Normal,
    Increase,
    Decrease,
    Congested,
}

/// Google Congestion Control (used in WebRTC)
pub struct GoogleCC {
    aimd: AimdRateControl,
    delay_detector: DelayBasedBweDetector,
    loss_detector: LossBasedBweDetector,
}

/// Additive Increase Multiplicative Decrease
struct AimdRateControl {
    current_bitrate: u64,
    max_bitrate: u64,
    min_bitrate: u64,
    increase_rate: f32,
    decrease_factor: f32,
}

impl AimdRateControl {
    fn increase(&mut self, delta: Duration) {
        let increase = (self.current_bitrate as f32 * self.increase_rate
            * delta.as_secs_f32()) as u64;
        self.current_bitrate = (self.current_bitrate + increase).min(self.max_bitrate);
    }

    fn decrease(&mut self) {
        self.current_bitrate = ((self.current_bitrate as f32 * self.decrease_factor) as u64)
            .max(self.min_bitrate);
    }
}

/// SCReAM - Self-Clocked Rate Adaptation for Multimedia
pub struct ScreamCC {
    target_bitrate: u64,
    queue_delay: Duration,
    target_queue_delay: Duration,
    cwnd: f32,
    bytes_acked: u64,
}

/// NADA - Network-Assisted Dynamic Adaptation
pub struct NadaCC {
    reference_delay: Duration,
    queuing_delay: Duration,
    loss_rate: f32,
    aggregated_congestion_signal: f32,
}

impl CongestionController {
    pub fn new(algorithm: CongestionAlgorithmType, initial_bitrate: u64) -> Self {
        let algorithm: Box<dyn CongestionAlgorithm> = match algorithm {
            CongestionAlgorithmType::GoogleCC => Box::new(GoogleCC::new(initial_bitrate)),
            CongestionAlgorithmType::SCReAM => Box::new(ScreamCC::new(initial_bitrate)),
            CongestionAlgorithmType::NADA => Box::new(NadaCC::new(initial_bitrate)),
        };

        Self {
            algorithm,
            target_bitrate: initial_bitrate,
            current_bitrate: initial_bitrate,
            rtt_estimator: RttEstimator::new(),
            loss_detector: LossDetector::new(),
            state: CongestionState::Normal,
        }
    }

    pub fn update_with_feedback(&mut self, feedback: &RtcpFeedback) {
        // Update based on RTCP feedback
        match feedback {
            RtcpFeedback::REMB { bitrate_bps, .. } => {
                self.target_bitrate = *bitrate_bps;
            }
            _ => {}
        }
    }

    pub fn update_with_stats(&mut self, stats: &RtpStats) {
        let rtt = self.rtt_estimator.estimate(&stats);
        let loss_rate = self.loss_detector.calculate_loss_rate(&stats);

        let update = self.algorithm.update(rtt, loss_rate, stats.received_bitrate);

        self.target_bitrate = update.new_bitrate;
        self.state = update.state;
    }

    pub fn target_bitrate(&self) -> u64 {
        self.target_bitrate
    }
}

pub enum CongestionAlgorithmType {
    GoogleCC,
    SCReAM,
    NADA,
}
```

---

### 3. Quality of Experience (QoE) Monitoring

**Priority:** 🔴 High
**Impact:** Essential for proactive quality management and SLA monitoring.

```rust
// crates/forge-engine/src/qoe.rs

use std::collections::VecDeque;

/// Quality of Experience metrics
#[derive(Debug, Clone, Serialize)]
pub struct QoeMetrics {
    /// Mean Opinion Score (1.0 - 5.0, where 5.0 is excellent)
    pub mos: f32,

    /// R-factor (0 - 100, ITU-T G.107 E-model)
    pub r_factor: f32,

    /// Jitter in milliseconds
    pub jitter_ms: f32,

    /// Packet loss percentage (0.0 - 100.0)
    pub packet_loss_percent: f32,

    /// Round-trip time in milliseconds
    pub rtt_ms: u32,

    /// Codec being used
    pub codec: Codec,

    /// Signal level in dBm
    pub signal_level_dbm: f32,

    /// Noise level in dBm
    pub noise_level_dbm: f32,

    /// Echo return loss in dB
    pub echo_return_loss_db: f32,

    /// Concealment events (PLC activations)
    pub concealment_events: u32,

    /// Timestamp of measurement
    pub timestamp: DateTime<Utc>,
}

pub struct QoeCalculator {
    codec_impairment: DashMap<Codec, f32>,
    window_size: usize,
    history: VecDeque<QoeMetrics>,
}

impl QoeCalculator {
    pub fn new() -> Self {
        let mut codec_impairment = DashMap::new();

        // ITU-T G.107 equipment impairment factors
        codec_impairment.insert(Codec::PCMU, 0.0);      // G.711 μ-law
        codec_impairment.insert(Codec::PCMA, 0.0);      // G.711 A-law
        codec_impairment.insert(Codec::G722, 2.0);      // G.722
        codec_impairment.insert(Codec::G729, 11.0);     // G.729
        codec_impairment.insert(Codec::Opus, 5.0);      // Opus (depends on bitrate)
        codec_impairment.insert(Codec::Speex, 8.0);     // Speex
        codec_impairment.insert(Codec::iLBC, 7.0);      // iLBC

        Self {
            codec_impairment,
            window_size: 10,
            history: VecDeque::with_capacity(10),
        }
    }

    /// Calculate MOS using ITU-T G.107 E-model
    pub fn calculate_mos(&self, stats: &RtpStats, codec: Codec) -> f32 {
        let r_factor = self.calculate_r_factor(stats, codec);

        // Convert R-factor to MOS (ITU-T G.107)
        if r_factor < 0.0 {
            1.0
        } else if r_factor > 100.0 {
            4.5
        } else {
            1.0 + 0.035 * r_factor + 7.0e-6 * r_factor * (r_factor - 60.0) * (100.0 - r_factor)
        }
    }

    /// Calculate R-factor (ITU-T G.107 E-model)
    pub fn calculate_r_factor(&self, stats: &RtpStats, codec: Codec) -> f32 {
        // R = R0 - Is - Id - Ie-eff + A
        // R0: Basic signal-to-noise ratio
        // Is: Simultaneous impairment (echo, sidetone)
        // Id: Delay impairment
        // Ie-eff: Equipment impairment (codec + packet loss)
        // A: Advantage factor (0 for wireline, up to 20 for mobile)

        let r0 = 94.2; // Typical for digital connections

        // Simultaneous impairment (echo)
        let echo_loss = stats.echo_return_loss_db.unwrap_or(65.0);
        let is = if echo_loss > 65.0 { 0.0 } else { 65.0 - echo_loss };

        // Delay impairment
        let delay_ms = stats.rtt_ms as f32 / 2.0 + stats.jitter_ms;
        let id = self.calculate_delay_impairment(delay_ms);

        // Equipment impairment
        let ie = self.codec_impairment.get(&codec).map(|v| *v).unwrap_or(10.0);
        let bpl = self.calculate_burst_packet_loss(stats);
        let ie_eff = ie + (95.0 - ie) * bpl / (bpl + 10.0);

        // Advantage factor (0 for VoIP)
        let a = 0.0;

        let r = r0 - is - id - ie_eff + a;
        r.max(0.0).min(100.0)
    }

    fn calculate_delay_impairment(&self, delay_ms: f32) -> f32 {
        // ITU-T G.107 delay impairment function
        let x = delay_ms.ln();
        0.024 * delay_ms + 0.11 * (delay_ms - 177.3).max(0.0)
    }

    fn calculate_burst_packet_loss(&self, stats: &RtpStats) -> f32 {
        // Bursty packet loss is worse than random loss
        let loss_rate = stats.packet_loss_percent;
        let burst_ratio = stats.burst_loss_ratio.unwrap_or(1.0);
        loss_rate * burst_ratio
    }

    /// Check quality thresholds and generate alerts
    pub fn check_thresholds(&self, metrics: &QoeMetrics) -> Vec<QualityAlert> {
        let mut alerts = Vec::new();

        // MOS thresholds
        if metrics.mos < 3.5 {
            alerts.push(QualityAlert::LowMos {
                mos: metrics.mos,
                threshold: 3.5,
                severity: if metrics.mos < 2.5 {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                },
            });
        }

        // Jitter threshold
        if metrics.jitter_ms > 30.0 {
            alerts.push(QualityAlert::HighJitter {
                value: metrics.jitter_ms,
                threshold: 30.0,
                severity: if metrics.jitter_ms > 50.0 {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                },
            });
        }

        // Packet loss threshold
        if metrics.packet_loss_percent > 1.0 {
            alerts.push(QualityAlert::PacketLoss {
                percent: metrics.packet_loss_percent,
                threshold: 1.0,
                severity: if metrics.packet_loss_percent > 3.0 {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                },
            });
        }

        // Latency threshold
        if metrics.rtt_ms > 200 {
            alerts.push(QualityAlert::HighLatency {
                rtt_ms: metrics.rtt_ms,
                threshold: 200,
                severity: if metrics.rtt_ms > 400 {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                },
            });
        }

        alerts
    }

    /// Get trending quality (improving, stable, degrading)
    pub fn get_trend(&self) -> QualityTrend {
        if self.history.len() < 3 {
            return QualityTrend::Stable;
        }

        let recent: Vec<f32> = self.history.iter().rev().take(3).map(|m| m.mos).collect();
        let avg_recent = recent.iter().sum::<f32>() / recent.len() as f32;

        let older: Vec<f32> = self.history.iter().rev().skip(3).take(3).map(|m| m.mos).collect();
        let avg_older = older.iter().sum::<f32>() / older.len() as f32;

        let delta = avg_recent - avg_older;

        if delta > 0.2 {
            QualityTrend::Improving
        } else if delta < -0.2 {
            QualityTrend::Degrading
        } else {
            QualityTrend::Stable
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum QualityAlert {
    LowMos {
        mos: f32,
        threshold: f32,
        severity: AlertSeverity,
    },
    HighJitter {
        value: f32,
        threshold: f32,
        severity: AlertSeverity,
    },
    PacketLoss {
        percent: f32,
        threshold: f32,
        severity: AlertSeverity,
    },
    HighLatency {
        rtt_ms: u32,
        threshold: u32,
        severity: AlertSeverity,
    },
    CodecMismatch {
        expected: Codec,
        actual: Codec,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum QualityTrend {
    Improving,
    Stable,
    Degrading,
}
```

**FCP Integration:**
- Emit QoE metrics to FCP event bus for real-time monitoring
- Store QoE data in CDRs for historical analysis
- Trigger FCP actions on quality degradation (e.g., reroute calls, alert ops)

---

### 4. Audio Processing DSP Pipeline

**Priority:** 🔴 High
**Impact:** Critical for quality - echo cancellation is essential for speakerphone/mobile scenarios.

```rust
// crates/forge-dsp/src/lib.rs

pub struct AudioProcessor {
    pipeline: Vec<Box<dyn AudioFilter>>,
    sample_rate: u32,
    frame_size: usize,
}

pub trait AudioFilter: Send + Sync {
    fn process(&mut self, samples: &mut [f32]) -> Result<(), DspError>;
    fn reset(&mut self);
    fn latency_samples(&self) -> usize;
}

impl AudioProcessor {
    pub fn new(sample_rate: u32, frame_size: usize) -> Self {
        Self {
            pipeline: Vec::new(),
            sample_rate,
            frame_size,
        }
    }

    pub fn add_filter(&mut self, filter: Box<dyn AudioFilter>) {
        self.pipeline.push(filter);
    }

    pub fn process(&mut self, samples: &mut [f32]) -> Result<(), DspError> {
        for filter in &mut self.pipeline {
            filter.process(samples)?;
        }
        Ok(())
    }

    pub fn total_latency_ms(&self) -> f32 {
        let total_samples: usize = self.pipeline.iter()
            .map(|f| f.latency_samples())
            .sum();
        (total_samples as f32 / self.sample_rate as f32) * 1000.0
    }
}
```

#### Echo Cancellation

```rust
// crates/forge-dsp/src/aec.rs

/// Acoustic Echo Cancellation
pub struct EchoCanceller {
    algorithm: Box<dyn AecAlgorithm>,
    tail_length_ms: u32,
    sample_rate: u32,
}

pub trait AecAlgorithm: Send + Sync {
    fn process(
        &mut self,
        near_end: &mut [f32],  // Microphone input
        far_end: &[f32],        // Speaker output (reference)
    ) -> Result<(), AecError>;
}

/// WebRTC AEC3 implementation (via C++ bindings)
pub struct WebRtcAec3 {
    handle: *mut libc::c_void,
    tail_length_samples: usize,
}

impl WebRtcAec3 {
    pub fn new(sample_rate: u32, tail_length_ms: u32) -> Result<Self, AecError> {
        let tail_length_samples = (sample_rate as f32 * tail_length_ms as f32 / 1000.0) as usize;

        unsafe {
            let config = webrtc_sys::aec3_create_config(sample_rate as i32);
            let handle = webrtc_sys::aec3_create(config);

            if handle.is_null() {
                return Err(AecError::InitializationFailed);
            }

            Ok(Self {
                handle,
                tail_length_samples,
            })
        }
    }
}

impl AecAlgorithm for WebRtcAec3 {
    fn process(&mut self, near_end: &mut [f32], far_end: &[f32]) -> Result<(), AecError> {
        unsafe {
            let result = webrtc_sys::aec3_process(
                self.handle,
                near_end.as_mut_ptr(),
                far_end.as_ptr(),
                near_end.len() as i32,
            );

            if result != 0 {
                return Err(AecError::ProcessingFailed);
            }
        }
        Ok(())
    }
}

/// Speex AEC implementation
pub struct SpeexAec {
    state: *mut speex_sys::SpeexEchoState,
    frame_size: usize,
    filter_length: usize,
}

impl SpeexAec {
    pub fn new(sample_rate: u32, frame_size: usize, filter_length_ms: u32) -> Result<Self, AecError> {
        let filter_length = (sample_rate as f32 * filter_length_ms as f32 / 1000.0) as usize;

        unsafe {
            let state = speex_sys::speex_echo_state_init(
                frame_size as i32,
                filter_length as i32,
            );

            if state.is_null() {
                return Err(AecError::InitializationFailed);
            }

            speex_sys::speex_echo_ctl(
                state,
                speex_sys::SPEEX_ECHO_SET_SAMPLING_RATE,
                &sample_rate as *const u32 as *mut libc::c_void,
            );

            Ok(Self {
                state,
                frame_size,
                filter_length,
            })
        }
    }
}

impl AecAlgorithm for SpeexAec {
    fn process(&mut self, near_end: &mut [f32], far_end: &[f32]) -> Result<(), AecError> {
        // Convert f32 to i16 for Speex
        let mut near_i16: Vec<i16> = near_end.iter()
            .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16)
            .collect();
        let far_i16: Vec<i16> = far_end.iter()
            .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16)
            .collect();

        unsafe {
            speex_sys::speex_echo_cancellation(
                self.state,
                near_i16.as_ptr(),
                far_i16.as_ptr(),
                near_i16.as_mut_ptr(),
            );
        }

        // Convert back to f32
        for (i, &sample) in near_i16.iter().enumerate() {
            near_end[i] = sample as f32 / 32768.0;
        }

        Ok(())
    }
}

impl EchoCanceller {
    pub fn new_webrtc(sample_rate: u32, tail_length_ms: u32) -> Result<Self, AecError> {
        Ok(Self {
            algorithm: Box::new(WebRtcAec3::new(sample_rate, tail_length_ms)?),
            tail_length_ms,
            sample_rate,
        })
    }

    pub fn new_speex(sample_rate: u32, frame_size: usize, tail_length_ms: u32) -> Result<Self, AecError> {
        Ok(Self {
            algorithm: Box::new(SpeexAec::new(sample_rate, frame_size, tail_length_ms)?),
            tail_length_ms,
            sample_rate,
        })
    }
}

impl AudioFilter for EchoCanceller {
    fn process(&mut self, samples: &mut [f32]) -> Result<(), DspError> {
        // Note: This simplified example assumes far_end is available
        // In practice, you'd need to pass it separately or maintain state
        Ok(())
    }

    fn reset(&mut self) {
        // Reset algorithm state
    }

    fn latency_samples(&self) -> usize {
        // AEC typically adds minimal latency
        64
    }
}
```

#### Noise Reduction

```rust
// crates/forge-dsp/src/noise_reduction.rs

pub struct NoiseReduction {
    algorithm: Box<dyn NrAlgorithm>,
    suppression_db: f32,
}

pub trait NrAlgorithm: Send + Sync {
    fn process(&mut self, samples: &mut [f32]) -> Result<(), NrError>;
}

/// WebRTC Noise Suppression
pub struct WebRtcNs {
    handle: *mut libc::c_void,
    level: i32,
}

impl WebRtcNs {
    pub fn new(sample_rate: u32, level: NsLevel) -> Result<Self, NrError> {
        unsafe {
            let handle = webrtc_sys::ns_create(sample_rate as i32);
            if handle.is_null() {
                return Err(NrError::InitializationFailed);
            }

            let level_int = match level {
                NsLevel::Low => 0,
                NsLevel::Moderate => 1,
                NsLevel::High => 2,
                NsLevel::VeryHigh => 3,
            };

            webrtc_sys::ns_set_level(handle, level_int);

            Ok(Self {
                handle,
                level: level_int,
            })
        }
    }
}

impl NrAlgorithm for WebRtcNs {
    fn process(&mut self, samples: &mut [f32]) -> Result<(), NrError> {
        unsafe {
            let result = webrtc_sys::ns_process(
                self.handle,
                samples.as_mut_ptr(),
                samples.len() as i32,
            );

            if result != 0 {
                return Err(NrError::ProcessingFailed);
            }
        }
        Ok(())
    }
}

/// RNNoise (ML-based noise suppression)
pub struct RNNoise {
    state: *mut rnnoise_sys::DenoiseState,
}

impl RNNoise {
    pub fn new() -> Result<Self, NrError> {
        unsafe {
            let state = rnnoise_sys::rnnoise_create(std::ptr::null_mut());
            if state.is_null() {
                return Err(NrError::InitializationFailed);
            }
            Ok(Self { state })
        }
    }
}

impl NrAlgorithm for RNNoise {
    fn process(&mut self, samples: &mut [f32]) -> Result<(), NrError> {
        // RNNoise operates on 480 samples (10ms at 48kHz)
        const FRAME_SIZE: usize = 480;

        for chunk in samples.chunks_mut(FRAME_SIZE) {
            if chunk.len() == FRAME_SIZE {
                unsafe {
                    rnnoise_sys::rnnoise_process_frame(
                        self.state,
                        chunk.as_mut_ptr(),
                        chunk.as_ptr(),
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum NsLevel {
    Low,
    Moderate,
    High,
    VeryHigh,
}
```

#### Automatic Gain Control

```rust
// crates/forge-dsp/src/agc.rs

pub struct AutomaticGainControl {
    target_level_dbfs: f32,
    compression_gain_db: f32,
    enable_limiter: bool,
    current_gain: f32,
    attack_time: Duration,
    release_time: Duration,
}

impl AutomaticGainControl {
    pub fn new(target_level_dbfs: f32, compression_gain_db: f32) -> Self {
        Self {
            target_level_dbfs,
            compression_gain_db,
            enable_limiter: true,
            current_gain: 1.0,
            attack_time: Duration::from_millis(5),
            release_time: Duration::from_millis(50),
        }
    }

    fn calculate_rms(&self, samples: &[f32]) -> f32 {
        let sum_squares: f32 = samples.iter().map(|&s| s * s).sum();
        (sum_squares / samples.len() as f32).sqrt()
    }

    fn rms_to_db(&self, rms: f32) -> f32 {
        20.0 * rms.max(1e-10).log10()
    }

    fn db_to_linear(&self, db: f32) -> f32 {
        10.0_f32.powf(db / 20.0)
    }
}

impl AudioFilter for AutomaticGainControl {
    fn process(&mut self, samples: &mut [f32]) -> Result<(), DspError> {
        let rms = self.calculate_rms(samples);
        let current_level_db = self.rms_to_db(rms);

        // Calculate required gain
        let gain_db = self.target_level_dbfs - current_level_db;
        let clamped_gain_db = gain_db.clamp(-self.compression_gain_db, self.compression_gain_db);
        let target_gain = self.db_to_linear(clamped_gain_db);

        // Smooth gain changes (attack/release)
        if target_gain > self.current_gain {
            // Release (slow increase)
            self.current_gain += (target_gain - self.current_gain) * 0.01;
        } else {
            // Attack (fast decrease)
            self.current_gain += (target_gain - self.current_gain) * 0.1;
        }

        // Apply gain
        for sample in samples.iter_mut() {
            *sample *= self.current_gain;

            // Limiter
            if self.enable_limiter {
                *sample = sample.clamp(-1.0, 1.0);
            }
        }

        Ok(())
    }

    fn reset(&mut self) {
        self.current_gain = 1.0;
    }

    fn latency_samples(&self) -> usize {
        0
    }
}
```

#### Equalizer

```rust
// crates/forge-dsp/src/equalizer.rs

pub struct EqualizerFilter {
    bands: Vec<BiquadFilter>,
}

pub struct EqBand {
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q_factor: f32,
    pub filter_type: FilterType,
}

pub enum FilterType {
    LowShelf,
    HighShelf,
    Peak,
    LowPass,
    HighPass,
    BandPass,
    Notch,
}

/// Biquad filter (2nd order IIR)
struct BiquadFilter {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BiquadFilter {
    fn new_peak(frequency: f32, sample_rate: f32, gain_db: f32, q: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let omega = 2.0 * std::f32::consts::PI * frequency / sample_rate;
        let sin_omega = omega.sin();
        let cos_omega = omega.cos();
        let alpha = sin_omega / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_omega;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_omega;
        let a2 = 1.0 - alpha / a;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn process_sample(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.b1 * self.x1 + self.b2 * self.x2
                   - self.a1 * self.y1 - self.a2 * self.y2;

        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = output;

        output
    }
}

impl AudioFilter for EqualizerFilter {
    fn process(&mut self, samples: &mut [f32]) -> Result<(), DspError> {
        for sample in samples.iter_mut() {
            let mut output = *sample;
            for band in &mut self.bands {
                output = band.process_sample(output);
            }
            *sample = output;
        }
        Ok(())
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            band.x1 = 0.0;
            band.x2 = 0.0;
            band.y1 = 0.0;
            band.y2 = 0.0;
        }
    }

    fn latency_samples(&self) -> usize {
        0
    }
}
```

**Configuration Example:**

```toml
[forge.dsp]
enabled = true

[forge.dsp.aec]
enabled = true
algorithm = "webrtc_aec3"  # or "speex"
tail_length_ms = 200

[forge.dsp.noise_reduction]
enabled = true
algorithm = "rnnoise"  # or "webrtc_ns"
level = "moderate"

[forge.dsp.agc]
enabled = true
target_level_dbfs = -18.0
compression_gain_db = 12.0
enable_limiter = true

[[forge.dsp.equalizer.bands]]
frequency_hz = 100.0
gain_db = 3.0
q_factor = 1.0
filter_type = "low_shelf"

[[forge.dsp.equalizer.bands]]
frequency_hz = 1000.0
gain_db = -2.0
q_factor = 1.5
filter_type = "peak"
```

---

### 5. Observability & Metrics (Prometheus/OpenTelemetry)

**Priority:** 🔴 High
**Impact:** Essential for production operations and SLA monitoring.

```rust
// crates/forge-observe/src/metrics.rs

use opentelemetry::metrics::{Meter, Counter, Histogram, ObservableGauge};
use prometheus::{Registry, Opts, HistogramOpts, HistogramVec, CounterVec, GaugeVec};

pub struct ForgeMetrics {
    registry: Registry,

    // Session metrics
    active_sessions: GaugeVec,
    session_duration: HistogramVec,
    sessions_total: CounterVec,
    sessions_failed: CounterVec,

    // Media quality metrics
    mos_score: HistogramVec,
    packet_loss_ratio: HistogramVec,
    jitter_ms: HistogramVec,
    rtt_ms: HistogramVec,

    // RTP metrics
    rtp_packets_sent: CounterVec,
    rtp_packets_received: CounterVec,
    rtp_bytes_sent: CounterVec,
    rtp_bytes_received: CounterVec,

    // Codec metrics
    codec_usage: CounterVec,
    transcoding_operations: CounterVec,

    // Resource utilization
    cpu_percent: Gauge,
    memory_bytes: Gauge,
    rtp_ports_used: Gauge,
    rtp_ports_available: Gauge,
    bandwidth_bps: GaugeVec,

    // Conference metrics
    conference_rooms: Gauge,
    conference_participants: GaugeVec,

    // Recording metrics
    recordings_active: Gauge,
    recording_bytes_written: Counter,

    // DTMF metrics
    dtmf_digits_detected: CounterVec,

    // AI streaming metrics
    ai_sessions_active: GaugeVec,
    ai_tokens_consumed: CounterVec,

    // Per-tenant metrics
    tenant_sessions: DashMap<TenantId, Gauge>,
    tenant_bandwidth: DashMap<TenantId, Gauge>,
}

impl ForgeMetrics {
    pub fn new() -> Result<Self, MetricsError> {
        let registry = Registry::new();

        // Session metrics
        let active_sessions = GaugeVec::new(
            Opts::new("forge_sessions_active", "Number of active media sessions"),
            &["tenant_id", "codec"],
        )?;
        registry.register(Box::new(active_sessions.clone()))?;

        let session_duration = HistogramVec::new(
            HistogramOpts::new("forge_session_duration_seconds", "Session duration")
                .buckets(vec![1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0]),
            &["tenant_id", "termination_reason"],
        )?;
        registry.register(Box::new(session_duration.clone()))?;

        let sessions_total = CounterVec::new(
            Opts::new("forge_sessions_total", "Total sessions created"),
            &["tenant_id", "codec"],
        )?;
        registry.register(Box::new(sessions_total.clone()))?;

        // Quality metrics
        let mos_score = HistogramVec::new(
            HistogramOpts::new("forge_mos_score", "Mean Opinion Score (1-5)")
                .buckets(vec![1.0, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0]),
            &["tenant_id", "codec"],
        )?;
        registry.register(Box::new(mos_score.clone()))?;

        let packet_loss_ratio = HistogramVec::new(
            HistogramOpts::new("forge_packet_loss_ratio", "Packet loss ratio (0-1)")
                .buckets(vec![0.0, 0.001, 0.005, 0.01, 0.02, 0.03, 0.05, 0.1]),
            &["tenant_id", "direction"],
        )?;
        registry.register(Box::new(packet_loss_ratio.clone()))?;

        let jitter_ms = HistogramVec::new(
            HistogramOpts::new("forge_jitter_milliseconds", "Jitter in milliseconds")
                .buckets(vec![0.0, 5.0, 10.0, 20.0, 30.0, 50.0, 100.0]),
            &["tenant_id"],
        )?;
        registry.register(Box::new(jitter_ms.clone()))?;

        let rtt_ms = HistogramVec::new(
            HistogramOpts::new("forge_rtt_milliseconds", "Round-trip time in milliseconds")
                .buckets(vec![0.0, 10.0, 25.0, 50.0, 100.0, 200.0, 500.0, 1000.0]),
            &["tenant_id"],
        )?;
        registry.register(Box::new(rtt_ms.clone()))?;

        // RTP metrics
        let rtp_packets_sent = CounterVec::new(
            Opts::new("forge_rtp_packets_sent_total", "Total RTP packets sent"),
            &["tenant_id", "codec"],
        )?;
        registry.register(Box::new(rtp_packets_sent.clone()))?;

        // ... (register remaining metrics)

        Ok(Self {
            registry,
            active_sessions,
            session_duration,
            sessions_total,
            // ... (initialize all fields)
        })
    }

    pub fn record_session_created(&self, tenant_id: &TenantId, codec: &Codec) {
        self.active_sessions
            .with_label_values(&[&tenant_id.to_string(), &codec.to_string()])
            .inc();
        self.sessions_total
            .with_label_values(&[&tenant_id.to_string(), &codec.to_string()])
            .inc();
    }

    pub fn record_session_ended(
        &self,
        tenant_id: &TenantId,
        codec: &Codec,
        duration: Duration,
        reason: &str,
    ) {
        self.active_sessions
            .with_label_values(&[&tenant_id.to_string(), &codec.to_string()])
            .dec();
        self.session_duration
            .with_label_values(&[&tenant_id.to_string(), reason])
            .observe(duration.as_secs_f64());
    }

    pub fn record_qoe_metrics(&self, tenant_id: &TenantId, codec: &Codec, qoe: &QoeMetrics) {
        self.mos_score
            .with_label_values(&[&tenant_id.to_string(), &codec.to_string()])
            .observe(qoe.mos as f64);
        self.packet_loss_ratio
            .with_label_values(&[&tenant_id.to_string(), "rx"])
            .observe(qoe.packet_loss_percent as f64 / 100.0);
        self.jitter_ms
            .with_label_values(&[&tenant_id.to_string()])
            .observe(qoe.jitter_ms as f64);
        self.rtt_ms
            .with_label_values(&[&tenant_id.to_string()])
            .observe(qoe.rtt_ms as f64);
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }
}
```

#### OpenTelemetry Tracing

```rust
// crates/forge-observe/src/tracing.rs

use opentelemetry::{
    trace::{Tracer, SpanKind, Status},
    KeyValue,
};
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub struct ForgeTracing {
    tracer: opentelemetry::sdk::trace::Tracer,
}

impl ForgeTracing {
    pub fn new(service_name: &str) -> Result<Self, TracingError> {
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(opentelemetry_otlp::new_exporter().tonic())
            .with_trace_config(
                opentelemetry::sdk::trace::config()
                    .with_resource(opentelemetry::sdk::Resource::new(vec![
                        KeyValue::new("service.name", service_name.to_string()),
                    ])),
            )
            .install_batch(opentelemetry::runtime::Tokio)?;

        Ok(Self { tracer })
    }

    /// Trace a media session lifecycle
    pub fn trace_session<F, R>(&self, call_id: &CallId, tenant_id: &TenantId, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let span = self.tracer.span_builder("forge.session")
            .with_kind(SpanKind::Server)
            .with_attributes(vec![
                KeyValue::new("call_id", call_id.to_string()),
                KeyValue::new("tenant_id", tenant_id.to_string()),
            ])
            .start(&self.tracer);

        let cx = opentelemetry::Context::current_with_span(span);
        let _guard = cx.attach();

        f()
    }
}
```

#### Metrics Export Endpoint

```rust
// crates/forge-api/src/metrics_api.rs

use axum::{routing::get, Router};
use prometheus::{Encoder, TextEncoder};

pub fn metrics_routes(metrics: Arc<ForgeMetrics>) -> Router {
    Router::new()
        .route("/metrics", get(prometheus_metrics))
        .layer(Extension(metrics))
}

async fn prometheus_metrics(
    Extension(metrics): Extension<Arc<ForgeMetrics>>,
) -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = metrics.registry().gather();
    let mut buffer = Vec::new();

    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to encode metrics: {}", e),
        ).into_response();
    }

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, encoder.format_type())],
        buffer,
    ).into_response()
}
```

**FCP Integration:**
- Export Forge metrics alongside FCP metrics at `/metrics` endpoint
- Use consistent label names (`tenant_id`, `call_id`) across FCP and Forge
- Integrate with FCP's tracing pipeline for distributed tracing

---

### 6. Multi-Tenancy & Resource Management

**Priority:** 🔴 High
**Impact:** Critical for SaaS/multi-tenant deployments.

```rust
// crates/forge-core/src/tenant.rs

use fcp_core::TenantId; // Use FCP's TenantId type!

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantConfig {
    pub tenant_id: TenantId,
    pub limits: ResourceLimits,
    pub qos_policy: QosPolicy,
    pub codec_policy: CodecPolicy,
    pub recording_policy: RecordingPolicy,
    pub billing_id: String,
    pub isolation: TenantIsolation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum concurrent sessions
    pub max_sessions: usize,

    /// Maximum bandwidth in bits per second
    pub max_bandwidth_bps: u64,

    /// Maximum recording minutes per month
    pub max_recording_minutes: u32,

    /// Maximum conference rooms
    pub max_conference_rooms: usize,

    /// Maximum participants per conference
    pub max_participants_per_conference: usize,

    /// Port pool allocation
    pub port_pool: Option<RangeInclusive<u16>>,

    /// Maximum transcoding sessions
    pub max_transcoding_sessions: usize,

    /// Storage quota in bytes
    pub max_storage_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QosPolicy {
    /// TOS/DSCP value for RTP packets
    pub tos: u8,

    /// Priority level (1-10, higher = more priority)
    pub priority: u8,

    /// Bandwidth reservation strategy
    pub reservation: BandwidthReservation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BandwidthReservation {
    /// No bandwidth reservation
    None,

    /// Reserve fixed bandwidth
    Fixed { bandwidth_bps: u64 },

    /// Reserve per-session bandwidth
    PerSession { bandwidth_bps: u64 },

    /// Burst allowance
    Burst {
        sustained_bps: u64,
        burst_bps: u64,
        burst_duration_secs: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodecPolicy {
    /// Allowed audio codecs
    pub allowed_audio_codecs: Vec<AudioCodec>,

    /// Allowed video codecs
    pub allowed_video_codecs: Vec<VideoCodec>,

    /// Preferred codec order
    pub preference_order: Vec<Codec>,

    /// Force transcoding for specific codecs
    pub force_transcode: Vec<Codec>,

    /// Block specific codecs
    pub blocked_codecs: Vec<Codec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingPolicy {
    /// Recording enabled for tenant
    pub enabled: bool,

    /// Auto-record all calls
    pub auto_record: bool,

    /// Recording format
    pub format: RecordingFormat,

    /// Storage backend
    pub storage_backend: StorageBackend,

    /// Retention policy
    pub retention_days: u32,

    /// Encryption required
    pub encryption_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantIsolation {
    /// Network isolation via VLAN
    pub vlan_id: Option<u16>,

    /// Subnet allocation
    pub subnet: Option<IpNetwork>,

    /// Storage isolation
    pub storage_bucket: String,
    pub storage_prefix: String,

    /// Encryption key ID (for KMS)
    pub encryption_key_id: String,

    /// Dedicated port range
    pub dedicated_ports: Option<RangeInclusive<u16>>,
}

/// Quota enforcement and resource tracking
pub struct QuotaEnforcer {
    quotas: DashMap<TenantId, TenantQuota>,
    usage: DashMap<TenantId, TenantUsage>,
}

#[derive(Debug, Clone)]
struct TenantQuota {
    config: TenantConfig,
    reservations: Vec<ResourceReservation>,
}

#[derive(Debug, Clone)]
pub struct TenantUsage {
    pub active_sessions: AtomicUsize,
    pub bandwidth_bps: AtomicU64,
    pub recording_minutes_used: AtomicU32,
    pub storage_bytes_used: AtomicU64,
    pub transcoding_sessions: AtomicUsize,
}

pub struct ResourceReservation {
    id: Uuid,
    resource: Resource,
    amount: u64,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
pub enum Resource {
    Session,
    Bandwidth { bps: u64 },
    RecordingMinutes { minutes: u32 },
    Storage { bytes: u64 },
    TranscodingSession,
    ConferenceRoom,
}

impl QuotaEnforcer {
    pub fn new() -> Self {
        Self {
            quotas: DashMap::new(),
            usage: DashMap::new(),
        }
    }

    pub fn register_tenant(&self, config: TenantConfig) {
        let tenant_id = config.tenant_id.clone();
        self.quotas.insert(
            tenant_id.clone(),
            TenantQuota {
                config,
                reservations: Vec::new(),
            },
        );
        self.usage.insert(
            tenant_id,
            TenantUsage {
                active_sessions: AtomicUsize::new(0),
                bandwidth_bps: AtomicU64::new(0),
                recording_minutes_used: AtomicU32::new(0),
                storage_bytes_used: AtomicU64::new(0),
                transcoding_sessions: AtomicUsize::new(0),
            },
        );
    }

    /// Check if admission is allowed for a resource
    pub fn check_admission(
        &self,
        tenant_id: &TenantId,
        resource: &Resource,
    ) -> Result<(), QuotaError> {
        let quota = self.quotas.get(tenant_id)
            .ok_or(QuotaError::TenantNotFound)?;
        let usage = self.usage.get(tenant_id)
            .ok_or(QuotaError::TenantNotFound)?;

        match resource {
            Resource::Session => {
                let current = usage.active_sessions.load(Ordering::SeqCst);
                if current >= quota.config.limits.max_sessions {
                    return Err(QuotaError::SessionLimitExceeded {
                        limit: quota.config.limits.max_sessions,
                        current,
                    });
                }
            }
            Resource::Bandwidth { bps } => {
                let current = usage.bandwidth_bps.load(Ordering::SeqCst);
                if current + bps > quota.config.limits.max_bandwidth_bps {
                    return Err(QuotaError::BandwidthLimitExceeded {
                        limit: quota.config.limits.max_bandwidth_bps,
                        current,
                        requested: *bps,
                    });
                }
            }
            Resource::RecordingMinutes { minutes } => {
                let current = usage.recording_minutes_used.load(Ordering::SeqCst);
                if current + minutes > quota.config.limits.max_recording_minutes {
                    return Err(QuotaError::RecordingQuotaExceeded);
                }
            }
            Resource::Storage { bytes } => {
                let current = usage.storage_bytes_used.load(Ordering::SeqCst);
                if current + bytes > quota.config.limits.max_storage_bytes {
                    return Err(QuotaError::StorageQuotaExceeded);
                }
            }
            Resource::TranscodingSession => {
                let current = usage.transcoding_sessions.load(Ordering::SeqCst);
                if current >= quota.config.limits.max_transcoding_sessions {
                    return Err(QuotaError::TranscodingLimitExceeded);
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Reserve a resource
    pub fn reserve(
        &self,
        tenant_id: &TenantId,
        resource: Resource,
        duration: Option<Duration>,
    ) -> Result<Reservation, QuotaError> {
        self.check_admission(tenant_id, &resource)?;

        let reservation_id = Uuid::new_v4();
        let expires_at = duration.map(|d| Instant::now() + d);

        // Update usage counters
        if let Some(usage) = self.usage.get(tenant_id) {
            match &resource {
                Resource::Session => {
                    usage.active_sessions.fetch_add(1, Ordering::SeqCst);
                }
                Resource::Bandwidth { bps } => {
                    usage.bandwidth_bps.fetch_add(*bps, Ordering::SeqCst);
                }
                Resource::TranscodingSession => {
                    usage.transcoding_sessions.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }

        Ok(Reservation {
            id: reservation_id,
            tenant_id: tenant_id.clone(),
            resource,
            expires_at,
        })
    }

    /// Release a reservation
    pub fn release(&self, reservation: &Reservation) {
        if let Some(usage) = self.usage.get(&reservation.tenant_id) {
            match &reservation.resource {
                Resource::Session => {
                    usage.active_sessions.fetch_sub(1, Ordering::SeqCst);
                }
                Resource::Bandwidth { bps } => {
                    usage.bandwidth_bps.fetch_sub(*bps, Ordering::SeqCst);
                }
                Resource::TranscodingSession => {
                    usage.transcoding_sessions.fetch_sub(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }

    pub fn get_usage(&self, tenant_id: &TenantId) -> Option<TenantUsageSnapshot> {
        self.usage.get(tenant_id).map(|usage| TenantUsageSnapshot {
            active_sessions: usage.active_sessions.load(Ordering::SeqCst),
            bandwidth_bps: usage.bandwidth_bps.load(Ordering::SeqCst),
            recording_minutes_used: usage.recording_minutes_used.load(Ordering::SeqCst),
            storage_bytes_used: usage.storage_bytes_used.load(Ordering::SeqCst),
            transcoding_sessions: usage.transcoding_sessions.load(Ordering::SeqCst),
        })
    }
}

pub struct Reservation {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub resource: Resource,
    pub expires_at: Option<Instant>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // Auto-release on drop
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TenantUsageSnapshot {
    pub active_sessions: usize,
    pub bandwidth_bps: u64,
    pub recording_minutes_used: u32,
    pub storage_bytes_used: u64,
    pub transcoding_sessions: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error("Tenant not found")]
    TenantNotFound,

    #[error("Session limit exceeded: {current}/{limit}")]
    SessionLimitExceeded { limit: usize, current: usize },

    #[error("Bandwidth limit exceeded: {current} + {requested} > {limit} bps")]
    BandwidthLimitExceeded {
        limit: u64,
        current: u64,
        requested: u64,
    },

    #[error("Recording quota exceeded")]
    RecordingQuotaExceeded,

    #[error("Storage quota exceeded")]
    StorageQuotaExceeded,

    #[error("Transcoding limit exceeded")]
    TranscodingLimitExceeded,
}
```

**FCP Integration:**
- Load tenant configs from FCP's configuration system
- Enforce quotas at both SIP (FCP) and media (Forge) layers
- Emit quota events to FCP event bus for billing integration

---

### 7. Security & Compliance Features

**Priority:** 🟡 Medium
**Impact:** Required for regulated industries (financial, healthcare, government).

#### Lawful Intercept

```rust
// crates/forge-security/src/lawful_intercept.rs

/// Lawful Intercept (CALEA/ETSI compliance)
pub struct LawfulIntercept {
    config: LiConfig,
    sessions: DashMap<CallId, LiSession>,
    delivery_functions: Vec<Arc<dyn DeliveryFunction>>,
}

#[derive(Debug, Clone)]
pub struct LiConfig {
    pub enabled: bool,
    pub compliance_standard: ComplianceStandard,
    pub case_id_prefix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplianceStandard {
    CALEA,  // US Communications Assistance for Law Enforcement Act
    ETSI,   // European Telecommunications Standards Institute
    LEMF,   // Law Enforcement Monitoring Facility
}

pub struct LiSession {
    pub case_id: String,
    pub call_id: CallId,
    pub target: InterceptTarget,
    pub start_time: DateTime<Utc>,
    pub delivery: Vec<Arc<dyn DeliveryFunction>>,
}

#[derive(Debug, Clone)]
pub struct InterceptTarget {
    pub subject_id: String,
    pub identifiers: Vec<String>, // Phone numbers, SIP URIs, etc.
    pub warrant_id: String,
    pub expiry: Option<DateTime<Utc>>,
}

/// Intercept-Related Information (IRI)
#[derive(Debug, Clone, Serialize)]
pub struct InterceptRelatedInfo {
    pub case_id: String,
    pub timestamp: DateTime<Utc>,
    pub event_type: IriEventType,
    pub calling_party: String,
    pub called_party: String,
    pub call_id: String,
    pub correlation_id: Uuid,
}

#[derive(Debug, Clone, Serialize)]
pub enum IriEventType {
    CallSetup,
    CallAnswer,
    CallEnd,
    CallForward,
    CallTransfer,
    SmsReceived,
    SmsSent,
}

/// Content of Communication (CC) - actual media
pub struct ContentOfCommunication {
    pub case_id: String,
    pub timestamp: DateTime<Utc>,
    pub stream_id: Uuid,
    pub direction: Direction,
    pub media_type: MediaType,
    pub codec: Codec,
    pub data: Vec<u8>,
}

#[async_trait]
pub trait DeliveryFunction: Send + Sync {
    async fn deliver_iri(&self, iri: InterceptRelatedInfo) -> Result<(), LiError>;
    async fn deliver_cc(&self, cc: ContentOfCommunication) -> Result<(), LiError>;
}

/// ETSI HI2 delivery (signaling)
pub struct EtsiHi2Delivery {
    endpoint: String,
    client: reqwest::Client,
}

#[async_trait]
impl DeliveryFunction for EtsiHi2Delivery {
    async fn deliver_iri(&self, iri: InterceptRelatedInfo) -> Result<(), LiError> {
        // Encode IRI as ETSI HI2 XML
        let xml = self.encode_hi2_xml(&iri)?;

        // Send to LEA (Law Enforcement Agency)
        self.client
            .post(&self.endpoint)
            .header("Content-Type", "application/xml")
            .body(xml)
            .send()
            .await?;

        Ok(())
    }

    async fn deliver_cc(&self, cc: ContentOfCommunication) -> Result<(), LiError> {
        // CC is delivered via HI3 (separate media stream)
        Ok(())
    }
}

/// ETSI HI3 delivery (media content)
pub struct EtsiHi3Delivery {
    rtp_endpoint: SocketAddr,
    socket: Arc<UdpSocket>,
}

#[async_trait]
impl DeliveryFunction for EtsiHi3Delivery {
    async fn deliver_iri(&self, _iri: InterceptRelatedInfo) -> Result<(), LiError> {
        // IRI is delivered via HI2
        Ok(())
    }

    async fn deliver_cc(&self, cc: ContentOfCommunication) -> Result<(), LiError> {
        // Forward media stream to LEA
        self.socket.send_to(&cc.data, self.rtp_endpoint).await?;
        Ok(())
    }
}

impl LawfulIntercept {
    pub async fn start_intercept(
        &self,
        call_id: &CallId,
        target: InterceptTarget,
    ) -> Result<(), LiError> {
        let case_id = format!("{}{}", self.config.case_id_prefix, Uuid::new_v4());

        let session = LiSession {
            case_id: case_id.clone(),
            call_id: call_id.clone(),
            target,
            start_time: Utc::now(),
            delivery: self.delivery_functions.clone(),
        };

        self.sessions.insert(call_id.clone(), session);

        // Send IRI: Call Setup
        let iri = InterceptRelatedInfo {
            case_id,
            timestamp: Utc::now(),
            event_type: IriEventType::CallSetup,
            // ... populate fields
            correlation_id: Uuid::new_v4(),
        };

        for delivery in &self.delivery_functions {
            delivery.deliver_iri(iri.clone()).await?;
        }

        Ok(())
    }

    pub async fn intercept_media(
        &self,
        call_id: &CallId,
        media: &[u8],
        direction: Direction,
    ) -> Result<(), LiError> {
        if let Some(session) = self.sessions.get(call_id) {
            let cc = ContentOfCommunication {
                case_id: session.case_id.clone(),
                timestamp: Utc::now(),
                stream_id: Uuid::new_v4(),
                direction,
                media_type: MediaType::Audio,
                codec: Codec::PCMU, // Or actual codec
                data: media.to_vec(),
            };

            for delivery in &session.delivery {
                delivery.deliver_cc(cc.clone()).await?;
            }
        }

        Ok(())
    }
}
```

#### STIR/SHAKEN

```rust
// crates/forge-security/src/stir_shaken.rs

/// STIR/SHAKEN call authentication
pub struct StirShaken {
    certificate: Certificate,
    private_key: PrivateKey,
    spc_token: String,
}

impl StirShaken {
    pub fn attest_call(
        &self,
        calling_number: &str,
        called_number: &str,
        call_id: &CallId,
    ) -> Result<String, StirShakenError> {
        let attestation = AttestationLevel::A; // Full attestation

        let passport = PassportClaim {
            attest: attestation,
            dest: Destination {
                tn: vec![called_number.to_string()],
            },
            iat: Utc::now().timestamp(),
            orig: Originator {
                tn: calling_number.to_string(),
            },
            orig_id: call_id.to_string(),
        };

        // Sign with private key
        let token = self.sign_passport(&passport)?;

        Ok(token)
    }

    pub fn verify_call(
        &self,
        identity_header: &str,
    ) -> Result<VerificationResult, StirShakenError> {
        // Parse Identity header
        let passport = self.parse_identity_header(identity_header)?;

        // Verify signature
        let signature_valid = self.verify_signature(&passport)?;

        // Check certificate validity
        let cert_valid = self.verify_certificate(&passport)?;

        Ok(VerificationResult {
            signature_valid,
            cert_valid,
            attestation: passport.attest,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttestationLevel {
    A,  // Full attestation - service provider attests
    B,  // Partial attestation - customer relationship but not call origination
    C,  // Gateway attestation - no relationship with customer
}
```

---

## FCP Integration Improvements

### 1. Type System Alignment

**Priority:** 🔴 High
**Impact:** Essential for clean integration with FCP.

```rust
// crates/forge-core/src/identifiers.rs

// Re-export FCP's core types
pub use fcp_core::{CallId, SessionId, TenantId, DialogId, TransactionId};

// Define Forge-specific identifiers using FCP's pattern
use fcp_core::define_id;

define_id!(MediaSessionId, "msess");
define_id!(ConferenceRoomId, "room");
define_id!(RecordingId, "rec");
define_id!(TranscriptionSessionId, "xscr");
define_id!(AiSessionId, "aisess");
define_id!(SiprecSessionId, "siprec");

// Conversion utilities
impl From<CallId> for MediaSessionId {
    fn from(call_id: CallId) -> Self {
        // Derive media session ID from call ID
        MediaSessionId::from_uuid(*call_id.as_uuid())
    }
}
```

### 2. Service Integration

**Priority:** 🔴 High
**Impact:** Allows FCP runtime to manage Forge lifecycle.

```rust
// crates/forge-api/src/service.rs

use fcp_core::service::{Service, ServiceContext};
use fcp_runtime::Runtime;
use async_trait::async_trait;

pub struct ForgeService {
    engine: Arc<ForgeEngine>,
    config: ForgeConfig,
    state: ServiceState,
}

#[async_trait]
impl Service for ForgeService {
    fn name(&self) -> &str {
        "forge-media"
    }

    async fn start(&mut self, ctx: &ServiceContext) -> fcp_core::Result<()> {
        tracing::info!("Starting Forge media engine");

        // Initialize transport
        self.engine.init_transport().await
            .map_err(|e| fcp_core::Error::Media(e.to_string()))?;

        // Start control API
        self.engine.start_control_api().await
            .map_err(|e| fcp_core::Error::Media(e.to_string()))?;

        // Start metrics exporter
        self.engine.start_metrics_export().await
            .map_err(|e| fcp_core::Error::Media(e.to_string()))?;

        self.state = ServiceState::Running;

        tracing::info!(
            port_range = ?self.config.port_range,
            "Forge media engine started"
        );

        Ok(())
    }

    async fn stop(&mut self, ctx: &ServiceContext) -> fcp_core::Result<()> {
        tracing::info!("Stopping Forge media engine");

        // Drain sessions gracefully
        self.engine.drain_sessions(ctx.shutdown_timeout()).await
            .map_err(|e| fcp_core::Error::Media(e.to_string()))?;

        // Close ports and cleanup
        self.engine.shutdown().await
            .map_err(|e| fcp_core::Error::Media(e.to_string()))?;

        self.state = ServiceState::Stopped;

        tracing::info!("Forge media engine stopped");

        Ok(())
    }

    async fn health_check(&self) -> fcp_core::Result<()> {
        // Check critical resources
        if self.engine.available_ports() < 100 {
            return Err(fcp_core::Error::Resource(
                "Low RTP port availability".to_string()
            ));
        }

        // Check CPU usage
        if self.engine.cpu_usage_percent() > 90.0 {
            return Err(fcp_core::Error::Resource(
                "High CPU usage".to_string()
            ));
        }

        // Check memory usage
        if self.engine.memory_usage_percent() > 90.0 {
            return Err(fcp_core::Error::Resource(
                "High memory usage".to_string()
            ));
        }

        Ok(())
    }
}

// Extension trait for Runtime
pub trait RuntimeForgeExt {
    fn with_forge_media(self, config: ForgeConfig) -> Self;
}

impl RuntimeForgeExt for fcp_runtime::RuntimeBuilder {
    fn with_forge_media(mut self, config: ForgeConfig) -> Self {
        let engine = ForgeEngine::new(config.clone());
        let service = ForgeService {
            engine: Arc::new(engine),
            config,
            state: ServiceState::Stopped,
        };

        self = self.with_service(Box::new(service));
        self
    }
}

// Usage in fcp-server
// let runtime = RuntimeBuilder::new()
//     .with_forge_media(forge_config)
//     .build();
```

### 3. Event Bus Integration

**Priority:** 🔴 High
**Impact:** Enables event-driven architecture between FCP and Forge.

```rust
// crates/forge-engine/src/events.rs

use fcp_core::event::{Event, EventBus};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaEvent {
    SessionCreated {
        call_id: CallId,
        tenant_id: TenantId,
        codec: Codec,
        local_addr: SocketAddr,
        remote_addr: Option<SocketAddr>,
        timestamp: DateTime<Utc>,
    },
    SessionAnswered {
        call_id: CallId,
        codec: Codec,
        timestamp: DateTime<Utc>,
    },
    SessionEnded {
        call_id: CallId,
        duration_ms: u64,
        stats: MediaStats,
        termination_reason: TerminationReason,
        timestamp: DateTime<Utc>,
    },
    QualityAlert {
        call_id: CallId,
        alert: QualityAlert,
        timestamp: DateTime<Utc>,
    },
    QualityDegraded {
        call_id: CallId,
        mos_before: f32,
        mos_after: f32,
        trend: QualityTrend,
        timestamp: DateTime<Utc>,
    },
    DtmfDetected {
        call_id: CallId,
        digit: char,
        duration_ms: u32,
        timestamp: DateTime<Utc>,
    },
    RecordingStarted {
        recording_id: RecordingId,
        call_id: CallId,
        format: RecordingFormat,
        timestamp: DateTime<Utc>,
    },
    RecordingCompleted {
        recording_id: RecordingId,
        call_id: CallId,
        file_path: String,
        duration_ms: u64,
        file_size_bytes: u64,
        timestamp: DateTime<Utc>,
    },
    TranscriptionResult {
        session_id: TranscriptionSessionId,
        call_id: CallId,
        text: String,
        is_final: bool,
        confidence: f32,
        timestamp: DateTime<Utc>,
    },
    ConferenceJoined {
        room_id: ConferenceRoomId,
        participant_id: ParticipantId,
        timestamp: DateTime<Utc>,
    },
    ConferenceLeft {
        room_id: ConferenceRoomId,
        participant_id: ParticipantId,
        duration_ms: u64,
        timestamp: DateTime<Utc>,
    },
}

impl From<MediaEvent> for Event {
    fn from(event: MediaEvent) -> Self {
        Event::Media(event)
    }
}

impl ForgeEngine {
    pub async fn publish_event(&self, event: MediaEvent) {
        if let Some(event_bus) = &self.event_bus {
            if let Err(e) = event_bus.publish(event.into()).await {
                tracing::error!(error = ?e, "Failed to publish media event");
            }
        }
    }

    pub async fn handle_sip_event(&self, event: Event) {
        match event {
            Event::Dialog(dialog_event) => {
                self.handle_dialog_event(dialog_event).await;
            }
            Event::Transaction(tx_event) => {
                self.handle_transaction_event(tx_event).await;
            }
            _ => {}
        }
    }
}
```

### 4. Configuration System Integration

**Priority:** 🟡 Medium
**Impact:** Unified configuration across FCP and Forge.

```toml
# fcp.toml - Unified configuration

[services.media]
enabled = true
engine = "forge"

[services.media.forge]
port_range = { start = 30000, end = 40000 }
tos = 0xB8  # EF (Expedited Forwarding)
session_timeout_secs = 300

[[services.media.forge.interfaces]]
name = "eth0"
address = "192.168.1.100"
advertised_address = "203.0.113.50"

[services.media.forge.kernel_offload]
enabled = true
backend = "rtpengine"

[services.media.forge.dsp]
enabled = true

[services.media.forge.dsp.aec]
enabled = true
algorithm = "webrtc_aec3"
tail_length_ms = 200

[services.media.forge.dsp.noise_reduction]
enabled = true
algorithm = "rnnoise"

# Per-tenant media configuration
[[tenants]]
id = "tenant-acme-corp"

[tenants.media]
max_sessions = 1000
max_bandwidth_bps = 100_000_000
codec_policy = ["opus", "pcmu", "pcma"]
recording_enabled = true
recording_format = "opus"
recording_storage = "s3"
transcription_enabled = true
```

```rust
// crates/forge-core/src/config.rs

use fcp_core::config::Config as FcpConfig;

impl ForgeConfig {
    pub fn from_fcp_config(fcp_config: &FcpConfig) -> Result<Self, ConfigError> {
        // Extract Forge configuration from FCP config
        let media_config = fcp_config.services.get("media")
            .ok_or(ConfigError::MediaConfigMissing)?;

        // Parse Forge-specific settings
        // ...

        Ok(forge_config)
    }
}
```

### 5. Error Handling Alignment

**Priority:** 🟡 Medium
**Impact:** Consistent error propagation across FCP and Forge.

```rust
// crates/forge-core/src/error.rs

use fcp_core::Error as FcpError;

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("Port exhaustion: no available RTP ports")]
    PortExhaustion,

    #[error("Codec not supported: {0}")]
    CodecNotSupported(String),

    #[error("SRTP key negotiation failed")]
    SrtpKeyFailure,

    #[error("Transcoding failed: {0}")]
    TranscodingFailed(String),

    #[error("Conference error: {0}")]
    ConferenceError(String),

    #[error("Recording error: {0}")]
    RecordingError(String),

    #[error("DSP error: {0}")]
    DspError(String),

    #[error("Quota exceeded: {0}")]
    QuotaExceeded(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<ForgeError> for FcpError {
    fn from(e: ForgeError) -> Self {
        match e {
            ForgeError::PortExhaustion => {
                FcpError::Resource("No available RTP ports".to_string())
            }
            ForgeError::CodecNotSupported(codec) => {
                FcpError::Media(format!("Unsupported codec: {}", codec))
            }
            ForgeError::SrtpKeyFailure => {
                FcpError::Media("SRTP key negotiation failed".to_string())
            }
            ForgeError::QuotaExceeded(msg) => {
                FcpError::Resource(msg)
            }
            _ => FcpError::Media(e.to_string()),
        }
    }
}
```

---

## Deployment & Operations

### 1. Kubernetes Operator

**Priority:** 🟡 Medium
**Impact:** Simplifies deployment and scaling in Kubernetes.

```rust
// forge-operator/src/crd.rs

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "forge.fcp.io",
    version = "v1",
    kind = "ForgeCluster",
    namespaced
)]
pub struct ForgeClusterSpec {
    pub replicas: i32,
    pub port_range: PortRange,
    pub ha_enabled: bool,
    pub storage: StorageConfig,
    pub image: String,
    pub resources: ResourceRequirements,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct StorageConfig {
    pub state_backend: String,  // "redis", "etcd"
    pub connection_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct ForgeClusterStatus {
    pub ready_replicas: i32,
    pub current_sessions: i32,
    pub phase: String,
}
```

### 2. Helm Charts

```yaml
# helm/forge/values.yaml

replicaCount: 3

image:
  repository: forge-media
  pullPolicy: IfNotPresent
  tag: "0.1.0"

service:
  type: LoadBalancer
  annotations:
    service.beta.kubernetes.io/aws-load-balancer-type: "nlb"

resources:
  limits:
    cpu: 4000m
    memory: 8Gi
  requests:
    cpu: 2000m
    memory: 4Gi

autoscaling:
  enabled: true
  minReplicas: 2
  maxReplicas: 10
  targetCPUUtilizationPercentage: 70
  targetMemoryUtilizationPercentage: 80

forge:
  portRange:
    start: 30000
    end: 40000
  tos: 0xB8

  ha:
    enabled: true
    storage:
      type: redis
      cluster: true
      url: redis://redis-cluster:6379

  observability:
    prometheus:
      enabled: true
      port: 9090
    tracing:
      enabled: true
      endpoint: "http://jaeger-collector:14268/api/traces"

persistence:
  enabled: true
  storageClass: "standard"
  size: 100Gi
```

---

## Priority Matrix

### 🔴 High Priority (Must-Have for Carrier-Grade)

| Feature | Impact | Effort | FCP Integration |
|---------|--------|--------|-----------------|
| Video Support | Critical - modern UC requires it | High | Medium - SDP negotiation |
| RTCP Feedback & Congestion Control | Critical for video quality | Medium | Low |
| QoE Monitoring & MOS Calculation | Essential for SLA monitoring | Medium | High - event bus, CDRs |
| Audio DSP (AEC, Noise Reduction) | Critical for quality | High | Low |
| Observability (Prometheus/OTel) | Essential for operations | Medium | High - unified metrics |
| Multi-Tenancy & Quotas | Critical for SaaS | High | High - tenant system |
| Type System Alignment | Clean integration with FCP | Low | High |
| Service Integration | Lifecycle management | Low | High |
| Event Bus Integration | Event-driven architecture | Medium | High |

### 🟡 Medium Priority (Important for Enterprise)

| Feature | Impact | Effort | FCP Integration |
|---------|--------|--------|-----------------|
| Lawful Intercept | Required for regulated industries | High | Medium - coordination |
| Emergency Services (E911) | Required for carriers | Medium | Medium |
| STIR/SHAKEN | Anti-spoofing for carriers | Medium | Medium |
| Configuration System Integration | Unified config | Low | High |
| Error Handling Alignment | Consistent error propagation | Low | High |
| Kubernetes Operator | Simplifies deployment | Medium | Low |
| Helm Charts | Standard packaging | Low | Low |

### 🟢 Nice-to-Have (Future Enhancements)

| Feature | Impact | Effort | FCP Integration |
|---------|--------|--------|-----------------|
| ML-based Noise Reduction | Better quality | Medium | Low |
| Video Layout Templates | Better conferencing UX | Low | Low |
| Screen Sharing | Modern collaboration | Medium | Low |
| Admin Dashboard UI | Operations visibility | High | Medium |
| Migration Tools | Easier adoption | Medium | Low |
| PSTN Gateway (H.248/MGCP) | Legacy interconnect | High | Medium |

---

## Implementation Roadmap

### Phase 1: Core Quality & Observability (4-6 weeks)

**Goal:** Production-ready monitoring and quality management

- [ ] QoE monitoring & MOS calculation
- [ ] Prometheus metrics export
- [ ] OpenTelemetry tracing integration
- [ ] Quality alerts and thresholds
- [ ] RTCP feedback handling (NACK, FIR, PLI, REMB)
- [ ] Basic congestion control (Google CC)

**FCP Integration:**
- Emit QoE events to FCP event bus
- Export metrics at `/metrics` endpoint
- Distributed tracing across SIP and media layers

### Phase 2: FCP Integration (2-3 weeks)

**Goal:** Seamless integration with FCP runtime

- [ ] Type system alignment (use FCP's CallId, TenantId, etc.)
- [ ] Implement fcp_core::Service trait
- [ ] Event bus integration
- [ ] Configuration system integration
- [ ] Error handling alignment
- [ ] Service registration with FCP runtime

### Phase 3: Multi-Tenancy & Resource Management (3-4 weeks)

**Goal:** SaaS-ready multi-tenancy

- [ ] Tenant configuration system
- [ ] Resource limits and quotas
- [ ] Quota enforcement
- [ ] Per-tenant metrics
- [ ] Tenant isolation (network, storage, encryption)
- [ ] Bandwidth reservation and QoS

### Phase 4: Audio DSP Pipeline (4-6 weeks)

**Goal:** Carrier-grade audio quality

- [ ] Echo cancellation (WebRTC AEC3, Speex)
- [ ] Noise reduction (WebRTC NS, RNNoise)
- [ ] Automatic Gain Control
- [ ] Equalizer filter
- [ ] Audio processing pipeline
- [ ] DSP configuration and tuning

### Phase 5: Video Support (6-8 weeks)

**Goal:** Complete video conferencing platform

- [ ] Video codec support (H.264, VP8, VP9)
- [ ] Video transcoding pipeline
- [ ] Video conferencing layouts
- [ ] Video recording
- [ ] Simulcast and SVC support
- [ ] Bandwidth adaptation

### Phase 6: Compliance & Security (4-6 weeks)

**Goal:** Regulatory compliance

- [ ] Lawful intercept (CALEA/ETSI)
- [ ] STIR/SHAKEN call authentication
- [ ] Emergency services (E911/NG911)
- [ ] Encryption key rotation
- [ ] Audit logging
- [ ] Compliance reporting

### Phase 7: Operations & Deployment (2-3 weeks)

**Goal:** Production deployment tooling

- [ ] Kubernetes operator
- [ ] Helm charts
- [ ] Auto-scaling policies
- [ ] Health checks and readiness probes
- [ ] Deployment documentation
- [ ] Operational runbooks

---

## Success Metrics

### Quality Metrics
- MOS > 4.0 for 95% of calls
- Packet loss < 1% for 99% of calls
- Jitter < 30ms for 95% of calls
- Echo return loss > 40dB

### Performance Metrics
- Support 10,000+ concurrent sessions per node
- Session setup latency < 100ms
- Media latency < 150ms end-to-end
- CPU usage < 70% at max capacity

### Reliability Metrics
- 99.99% uptime (52 minutes downtime/year)
- Failover time < 5 seconds
- Zero data loss during failover
- Graceful degradation under load

### Observability Metrics
- 100% of sessions have quality metrics
- Alert latency < 10 seconds
- Metrics retention: 30 days
- Distributed trace sampling: 1%

---

## Conclusion

Forge has a strong foundation with comprehensive audio features, SIPREC, AI streaming, and HA support. The recommended enhancements will make it truly carrier/enterprise-grade:

**Critical Additions:**
1. Video support (H.264, VP8, VP9)
2. Audio DSP (echo cancellation, noise reduction)
3. QoE monitoring and quality alerts
4. Prometheus/OpenTelemetry observability
5. Multi-tenancy with quotas and isolation
6. Type system alignment with FCP

**FCP Integration Benefits:**
- Unified type system and error handling
- Event-driven architecture
- Shared observability infrastructure
- Consistent configuration
- Coordinated lifecycle management

With these enhancements, Forge will be:
- **Carrier-grade:** Video, QoE monitoring, lawful intercept, E911
- **Enterprise-ready:** Multi-tenancy, quotas, compliance features
- **Production-ready:** Comprehensive observability, alerting, HA
- **FCP-integrated:** Seamless integration while maintaining standalone usability

The recommended implementation roadmap prioritizes core quality and observability first, followed by FCP integration, then advanced features like video and compliance.
