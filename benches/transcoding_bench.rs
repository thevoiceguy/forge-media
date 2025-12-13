//! Performance benchmarks for transcoding functionality
//!
//! These benchmarks measure transcoding latency, CPU usage, and memory patterns
//! for various codec pairs to ensure we meet the <5ms target.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use forge_codecs::{g711::{G711ALaw, G711MuLaw}, AudioCodec, AudioCodecType, AudioFormat};
use forge_resampler::Resampler;
use forge_transcoder::Transcoder;

#[cfg(feature = "opus")]
use forge_codecs::opus::{OpusCodec, OpusConfig};

/// Standard RTP frame size (20ms of audio at 8kHz = 160 samples)
const FRAME_SIZE_8KHZ: usize = 160;

/// Frame size for Opus (20ms at 48kHz = 960 samples)
const FRAME_SIZE_48KHZ: usize = 960;

/// Generate test audio samples (simple sine wave)
fn generate_test_samples(count: usize, frequency: f32, sample_rate: u32) -> Vec<i16> {
    (0..count)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let amplitude = 8000.0;
            (amplitude * (2.0 * std::f32::consts::PI * frequency * t).sin()) as i16
        })
        .collect()
}

/// Generate PCMU encoded frame (20ms)
fn generate_pcmu_frame() -> Vec<u8> {
    let samples = generate_test_samples(FRAME_SIZE_8KHZ, 440.0, 8000);
    let mut encoder = G711MuLaw::new(8000);
    encoder.encode(&samples).expect("Failed to encode PCMU")
}

/// Generate PCMA encoded frame (20ms)
fn generate_pcma_frame() -> Vec<u8> {
    let samples = generate_test_samples(FRAME_SIZE_8KHZ, 440.0, 8000);
    let mut encoder = G711ALaw::new(8000);
    encoder.encode(&samples).expect("Failed to encode PCMA")
}

/// Generate Opus encoded frame (20ms)
#[cfg(feature = "opus")]
fn generate_opus_frame() -> Vec<u8> {
    let samples = generate_test_samples(FRAME_SIZE_48KHZ, 440.0, 48000);
    let config = OpusConfig {
        sample_rate: 48000,
        channels: 1,
        ..Default::default()
    };
    let mut encoder = OpusCodec::with_config(config).expect("Failed to create Opus encoder");
    encoder.encode(&samples).expect("Failed to encode Opus")
}

/// Benchmark PCMU → PCMA transcoding (same sample rate)
fn bench_pcmu_to_pcma(c: &mut Criterion) {
    let mut group = c.benchmark_group("transcode_g711");
    group.throughput(Throughput::Elements(1)); // 1 frame per iteration

    let src_format = AudioFormat::new(8000, 1, AudioCodecType::PCMU);
    let dst_format = AudioFormat::new(8000, 1, AudioCodecType::PCMA);

    let pcmu_frame = generate_pcmu_frame();

    group.bench_function("pcmu_to_pcma", |b| {
        b.iter(|| {
            let mut transcoder = Transcoder::new(src_format, dst_format).expect("Failed to create transcoder");
            black_box(transcoder.transcode(black_box(&pcmu_frame)).expect("Transcode failed"))
        });
    });

    group.finish();
}

/// Benchmark PCMA → PCMU transcoding
fn bench_pcma_to_pcmu(c: &mut Criterion) {
    let mut group = c.benchmark_group("transcode_g711");
    group.throughput(Throughput::Elements(1));

    let src_format = AudioFormat::new(8000, 1, AudioCodecType::PCMA);
    let dst_format = AudioFormat::new(8000, 1, AudioCodecType::PCMU);

    let pcma_frame = generate_pcma_frame();

    group.bench_function("pcma_to_pcmu", |b| {
        b.iter(|| {
            let mut transcoder = Transcoder::new(src_format, dst_format).expect("Failed to create transcoder");
            black_box(transcoder.transcode(black_box(&pcma_frame)).expect("Transcode failed"))
        });
    });

    group.finish();
}

/// Benchmark Opus → PCMU transcoding (with resampling 48kHz → 8kHz)
#[cfg(feature = "opus")]
fn bench_opus_to_pcmu(c: &mut Criterion) {
    let mut group = c.benchmark_group("transcode_opus");
    group.throughput(Throughput::Elements(1));

    let src_format = AudioFormat::new(48000, 1, AudioCodecType::Opus);
    let dst_format = AudioFormat::new(8000, 1, AudioCodecType::PCMU);

    let opus_frame = generate_opus_frame();

    group.bench_function("opus_to_pcmu", |b| {
        b.iter(|| {
            let mut transcoder = Transcoder::new(src_format, dst_format).expect("Failed to create transcoder");
            black_box(transcoder.transcode(black_box(&opus_frame)).expect("Transcode failed"))
        });
    });

    group.finish();
}

/// Benchmark PCMU → Opus transcoding (with resampling 8kHz → 48kHz)
#[cfg(feature = "opus")]
fn bench_pcmu_to_opus(c: &mut Criterion) {
    let mut group = c.benchmark_group("transcode_opus");
    group.throughput(Throughput::Elements(1));

    let src_format = AudioFormat::new(8000, 1, AudioCodecType::PCMU);
    let dst_format = AudioFormat::new(48000, 1, AudioCodecType::Opus);

    let pcmu_frame = generate_pcmu_frame();

    group.bench_function("pcmu_to_opus", |b| {
        b.iter(|| {
            let mut transcoder = Transcoder::new(src_format, dst_format).expect("Failed to create transcoder");
            black_box(transcoder.transcode(black_box(&pcmu_frame)).expect("Transcode failed"))
        });
    });

    group.finish();
}

/// Benchmark resampling alone (8kHz → 48kHz)
fn bench_resampling_8k_to_48k(c: &mut Criterion) {
    let mut group = c.benchmark_group("resampling");
    group.throughput(Throughput::Elements(FRAME_SIZE_8KHZ as u64));

    let samples = generate_test_samples(FRAME_SIZE_8KHZ, 440.0, 8000);

    group.bench_function("8khz_to_48khz", |b| {
        b.iter(|| {
            let mut resampler = Resampler::new(8000, 48000, 1).expect("Failed to create resampler");
            black_box(resampler.resample(black_box(&samples)).expect("Resample failed"))
        });
    });

    group.finish();
}

/// Benchmark resampling (48kHz → 8kHz)
fn bench_resampling_48k_to_8k(c: &mut Criterion) {
    let mut group = c.benchmark_group("resampling");
    group.throughput(Throughput::Elements(FRAME_SIZE_48KHZ as u64));

    let samples = generate_test_samples(FRAME_SIZE_48KHZ, 440.0, 48000);

    group.bench_function("48khz_to_8khz", |b| {
        b.iter(|| {
            let mut resampler = Resampler::new(48000, 8000, 1).expect("Failed to create resampler");
            black_box(resampler.resample(black_box(&samples)).expect("Resample failed"))
        });
    });

    group.finish();
}

/// Benchmark decoder only (PCMU decode)
fn bench_decode_pcmu(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");
    group.throughput(Throughput::Bytes(FRAME_SIZE_8KHZ as u64));

    let pcmu_frame = generate_pcmu_frame();

    group.bench_function("pcmu", |b| {
        b.iter(|| {
            let mut decoder = G711MuLaw::new(8000);
            black_box(decoder.decode(black_box(&pcmu_frame)).expect("Decode failed"))
        });
    });

    group.finish();
}

/// Benchmark encoder only (PCMA encode)
fn bench_encode_pcma(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode");
    group.throughput(Throughput::Elements(FRAME_SIZE_8KHZ as u64));

    let samples = generate_test_samples(FRAME_SIZE_8KHZ, 440.0, 8000);

    group.bench_function("pcma", |b| {
        b.iter(|| {
            let mut encoder = G711ALaw::new(8000);
            black_box(encoder.encode(black_box(&samples)).expect("Encode failed"))
        });
    });

    group.finish();
}

/// Benchmark transcoder initialization (to measure setup overhead)
fn bench_transcoder_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("transcoder_init");

    group.bench_function("pcmu_to_pcma", |b| {
        b.iter(|| {
            let src_format = AudioFormat::new(8000, 1, AudioCodecType::PCMU);
            let dst_format = AudioFormat::new(8000, 1, AudioCodecType::PCMA);
            black_box(Transcoder::new(src_format, dst_format).expect("Failed to create transcoder"))
        });
    });

    #[cfg(feature = "opus")]
    group.bench_function("opus_to_pcmu", |b| {
        b.iter(|| {
            let src_format = AudioFormat::new(48000, 1, AudioCodecType::Opus);
            let dst_format = AudioFormat::new(8000, 1, AudioCodecType::PCMU);
            black_box(Transcoder::new(src_format, dst_format).expect("Failed to create transcoder"))
        });
    });

    group.finish();
}

/// Benchmark sustained transcoding (multiple frames in sequence)
fn bench_sustained_transcoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("sustained_transcode");
    group.throughput(Throughput::Elements(100)); // 100 frames = 2 seconds of audio

    let pcmu_frames: Vec<Vec<u8>> = (0..100).map(|_| generate_pcmu_frame()).collect();

    group.bench_function("pcmu_to_pcma_100frames", |b| {
        b.iter(|| {
            let src_format = AudioFormat::new(8000, 1, AudioCodecType::PCMU);
            let dst_format = AudioFormat::new(8000, 1, AudioCodecType::PCMA);
            let mut transcoder = Transcoder::new(src_format, dst_format).expect("Failed to create transcoder");

            for frame in &pcmu_frames {
                black_box(transcoder.transcode(black_box(frame)).expect("Transcode failed"));
            }
        });
    });

    group.finish();
}

/// Benchmark reusable transcoder (tests transcoder reuse)
fn bench_reusable_transcoder(c: &mut Criterion) {
    let mut group = c.benchmark_group("reusable_transcode");

    let pcmu_frame = generate_pcmu_frame();

    group.bench_function("pcmu_to_pcma_reuse", |b| {
        let src_format = AudioFormat::new(8000, 1, AudioCodecType::PCMU);
        let dst_format = AudioFormat::new(8000, 1, AudioCodecType::PCMA);
        let mut transcoder = Transcoder::new(src_format, dst_format).expect("Failed to create transcoder");

        b.iter(|| {
            // Benchmark just the transcode operation, not the transcoder creation
            black_box(transcoder.transcode(black_box(&pcmu_frame)).expect("Transcode failed"))
        });
    });

    group.finish();
}

// Configure benchmark groups
#[cfg(feature = "opus")]
criterion_group!(
    benches,
    bench_pcmu_to_pcma,
    bench_pcma_to_pcmu,
    bench_opus_to_pcmu,
    bench_pcmu_to_opus,
    bench_resampling_8k_to_48k,
    bench_resampling_48k_to_8k,
    bench_decode_pcmu,
    bench_encode_pcma,
    bench_transcoder_creation,
    bench_sustained_transcoding,
    bench_reusable_transcoder
);

#[cfg(not(feature = "opus"))]
criterion_group!(
    benches,
    bench_pcmu_to_pcma,
    bench_pcma_to_pcmu,
    bench_resampling_8k_to_48k,
    bench_resampling_48k_to_8k,
    bench_decode_pcmu,
    bench_encode_pcma,
    bench_transcoder_creation,
    bench_sustained_transcoding,
    bench_reusable_transcoder
);

criterion_main!(benches);
