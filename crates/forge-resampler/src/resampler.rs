//! Audio resampling
//!
//! Sample rate conversion for audio streams, needed when transcoding between
//! codecs with different rates (G.711 at 8 kHz, G.722 at 16 kHz, Opus at 48
//! kHz) and when playing audio recorded at one rate into a call at another.
//!
//! A band-limited (windowed-sinc) resampler. Each output sample is the input
//! convolved with a Kaiser-windowed sinc low-pass, cut just below the Nyquist
//! frequency of the lower of the two rates, so upsampling leaves no images
//! and downsampling no aliases. It streams: the last input samples are kept
//! between calls, so audio handed over in 20 ms frames comes out exactly as
//! if it had been handed over whole — no step at every frame boundary, which
//! is what the linear interpolation this replaced produced 50 times a second
//! (heard as a buzz under every prompt played to a G.722 phone). The price is
//! a constant delay of [`Resampler::delay_frames`] input frames, taken once
//! at the start of a stream (or after [`Resampler::reset`]); after it, every
//! input frame yields `dst_rate / src_rate` output frames on average, and
//! exactly that many when the ratio is an integer.

use crate::{ResamplerError, Result};

/// Zero crossings of the sinc on each side of the centre, at the cutoff.
const ZERO_CROSSINGS: f64 = 16.0;
/// Kaiser window shape: about 80 dB of stopband attenuation.
const KAISER_BETA: f64 = 8.0;
/// Fraction of the lower Nyquist frequency the passband keeps.
const ROLLOFF: f64 = 0.92;
/// Table resolution: kernel values per input sample of distance.
const TABLE_STEPS: usize = 512;

/// Audio resampler for sample rate conversion
pub struct Resampler {
    /// Source sample rate
    src_rate: u32,
    /// Destination sample rate
    dst_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo)
    channels: u16,
    /// Where the next output falls, in frames of the working buffer
    /// (history followed by the new input), counted exactly in units of
    /// `1 / unit` frame: whole outputs per frame stay whole, with no drift.
    position: u64,
    /// Units per input frame: the destination rate (over their divisor).
    unit: u64,
    /// Units per output frame: the source rate (over their divisor).
    step: u64,
    /// Taps on each side of the centre.
    half: usize,
    /// The kernel, one side, at `TABLE_STEPS` points per frame of distance,
    /// from the centre out to `half` (plus one guard point).
    table: Vec<f64>,
    /// The last `2 * half` input frames, interleaved, oldest first.
    history: Vec<f64>,
}

impl std::fmt::Debug for Resampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resampler")
            .field("src_rate", &self.src_rate)
            .field("dst_rate", &self.dst_rate)
            .field("channels", &self.channels)
            .field("taps", &(2 * self.half))
            .finish()
    }
}

impl Resampler {
    /// Create a new resampler
    ///
    /// # Arguments
    /// * `src_rate` - Input sample rate in Hz (e.g., 8000, 16000, 48000)
    /// * `dst_rate` - Output sample rate in Hz
    /// * `channels` - Number of channels (1 or 2)
    pub fn new(src_rate: u32, dst_rate: u32, channels: u16) -> Result<Self> {
        if src_rate == 0 || dst_rate == 0 {
            return Err(ResamplerError::InvalidFormat(
                "Sample rates must be greater than 0".to_string(),
            ));
        }

        if channels != 1 && channels != 2 {
            return Err(ResamplerError::InvalidFormat(
                "Only mono (1) and stereo (2) channels are supported".to_string(),
            ));
        }

        // The cutoff, in cycles per input sample (doubled: the sinc's
        // argument scale). Downsampling lowers it to the output's Nyquist.
        let scale = ROLLOFF * (dst_rate as f64 / src_rate as f64).min(1.0);
        let half = (ZERO_CROSSINGS / scale).ceil() as usize;
        let table = (0..=(half + 1) * TABLE_STEPS)
            .map(|i| kernel(i as f64 / TABLE_STEPS as f64, scale, half as f64))
            .collect();

        let divisor = gcd(src_rate as u64, dst_rate as u64);
        let mut resampler = Self {
            src_rate,
            dst_rate,
            channels,
            position: 0,
            unit: dst_rate as u64 / divisor,
            step: src_rate as u64 / divisor,
            half,
            table,
            history: Vec::new(),
        };
        resampler.reset();
        Ok(resampler)
    }

    /// Resample audio data
    ///
    /// # Arguments
    /// * `input` - Input PCM samples (16-bit signed, interleaved if stereo)
    ///
    /// # Returns
    /// Resampled PCM samples at the destination sample rate
    pub fn resample(&mut self, input: &[i16]) -> Result<Vec<i16>> {
        let channels = self.channels as usize;
        if input.len() % channels != 0 {
            return Err(ResamplerError::InvalidFormat(
                "Input length must be multiple of channel count".to_string(),
            ));
        }

        if input.is_empty() {
            return Ok(Vec::new());
        }

        // No resampling needed
        if self.src_rate == self.dst_rate {
            return Ok(input.to_vec());
        }

        let half = self.half;
        let mut buffer = std::mem::take(&mut self.history);
        buffer.extend(input.iter().map(|&s| s as f64));
        let frames = buffer.len() / channels;
        let input_frames = input.len() / channels;

        let mut output = Vec::with_capacity(
            (input_frames * self.unit as usize / self.step as usize + 1) * channels,
        );
        // An output centred at `position` needs `half` frames after it.
        while (self.position / self.unit) as usize + half < frames {
            let centre = (self.position / self.unit) as usize;
            let frac = (self.position % self.unit) as f64 / self.unit as f64;
            let first = centre + 1 - half;
            let mut acc = [0.0f64; 2];
            let mut norm = 0.0;
            for k in 0..2 * half {
                let frame = first + k;
                let distance = (frame as f64 - centre as f64 - frac).abs();
                let weight = self.weight(distance);
                norm += weight;
                for (ch, acc) in acc.iter_mut().enumerate().take(channels) {
                    *acc += weight * buffer[frame * channels + ch];
                }
            }
            for acc in acc.iter().take(channels) {
                // Normalised per output: the gain is exactly 1 at DC
                // whatever the fractional phase, so the level never ripples.
                let sample = (acc / norm).round().clamp(i16::MIN as f64, i16::MAX as f64);
                output.push(sample as i16);
            }
            self.position += self.step;
        }

        // Keep the last `2 * half` frames; outputs are placed against them.
        let keep = 2 * half;
        let dropped = frames - keep;
        self.history = buffer.split_off(dropped * channels);
        self.position -= dropped as u64 * self.unit;

        Ok(output)
    }

    /// The kernel at `distance` input frames from an output's centre.
    fn weight(&self, distance: f64) -> f64 {
        let at = distance * TABLE_STEPS as f64;
        let i = at as usize;
        if i + 1 >= self.table.len() {
            return 0.0;
        }
        let frac = at - i as f64;
        self.table[i] + (self.table[i + 1] - self.table[i]) * frac
    }

    /// Input frames between an input sample and the output it is centred
    /// on: the stream's delay.
    pub fn delay_frames(&self) -> usize {
        if self.src_rate == self.dst_rate {
            0
        } else {
            self.half
        }
    }

    /// Reset resampler state: the next call starts a new stream.
    pub fn reset(&mut self) {
        // Silence before the stream, so the first outputs have history.
        self.history = vec![0.0; 2 * self.half * self.channels as usize];
        // The first output is centred on the newest silent frame before the
        // stream's first sample shifted back by the delay: `half` frames in.
        self.position = self.half as u64 * self.unit;
    }

    /// Get source sample rate
    pub fn src_rate(&self) -> u32 {
        self.src_rate
    }

    /// Get destination sample rate
    pub fn dst_rate(&self) -> u32 {
        self.dst_rate
    }

    /// Get number of channels
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Check if resampling is needed (rates are different)
    pub fn needs_resampling(&self) -> bool {
        self.src_rate != self.dst_rate
    }
}

/// The windowed-sinc low-pass at `distance` frames: `scale` is the cutoff
/// relative to the input's Nyquist, `half` the window's half-width.
fn kernel(distance: f64, scale: f64, half: f64) -> f64 {
    if distance >= half {
        return 0.0;
    }
    let x = distance * scale;
    let sinc = if x == 0.0 {
        1.0
    } else {
        let pix = std::f64::consts::PI * x;
        pix.sin() / pix
    };
    let r = distance / half;
    let window = bessel_i0(KAISER_BETA * (1.0 - r * r).sqrt()) / bessel_i0(KAISER_BETA);
    scale * sinc * window
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// The modified Bessel function of the first kind, order 0 (its series).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half_x = x / 2.0;
    for k in 1..50 {
        term *= (half_x / k as f64).powi(2);
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

/// Resample a whole buffer at once — a recording, a prompt — with the
/// stream's delay taken out and its tail flushed: the output starts with the
/// input's first sample and lasts as long. Audio that arrives in pieces goes
/// through one [`Resampler`] kept for the stream instead.
pub fn resample_all(
    input: &[i16],
    src_rate: u32,
    dst_rate: u32,
    channels: u16,
) -> Result<Vec<i16>> {
    let mut resampler = Resampler::new(src_rate, dst_rate, channels)?;
    if !resampler.needs_resampling() {
        return Ok(input.to_vec());
    }
    let ch = channels as usize;
    if input.len() % ch != 0 {
        return Err(ResamplerError::InvalidFormat(
            "Input length must be multiple of channel count".to_string(),
        ));
    }
    let (unit, step) = (resampler.unit, resampler.step);
    // The first output centred exactly on the input's first sample (which
    // follows `2 * half` frames of silence), and as many as the input's
    // length calls for: the tail is flushed with silence after it.
    resampler.position = 2 * resampler.half as u64 * unit;
    let want = (input.len() as u64 / ch as u64 * unit).div_ceil(step) as usize;
    let mut padded = input.to_vec();
    padded.resize(input.len() + (resampler.half + 1) * ch, 0);
    let out = resampler.resample(&padded)?;
    Ok(out.into_iter().take(want * ch).collect())
}

/// Calculate the expected output length for a given input length
pub fn calculate_output_length(
    input_len: usize,
    src_rate: u32,
    dst_rate: u32,
    channels: u16,
) -> usize {
    if src_rate == 0 || dst_rate == 0 || channels == 0 {
        return 0;
    }
    let input_frames = input_len / channels as usize;
    let output_frames = ((input_frames as f64 * dst_rate as f64) / src_rate as f64).ceil() as usize;
    output_frames * channels as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn tone(freq: f64, rate: u32, seconds: f64, amplitude: f64) -> Vec<i16> {
        (0..(rate as f64 * seconds) as usize)
            .map(|n| (amplitude * (2.0 * PI * freq * n as f64 / rate as f64).sin()).round() as i16)
            .collect()
    }

    fn rms(samples: impl Iterator<Item = f64>) -> f64 {
        let (sum, n) = samples.fold((0.0, 0usize), |(s, n), x| (s + x * x, n + 1));
        (sum / n.max(1) as f64).sqrt()
    }

    /// The output against the ideal tone at the output rate, delayed by the
    /// resampler's delay: signal-to-error in dB, the start skipped.
    fn snr_db(out: &[i16], freq: f64, amplitude: f64, src: u32, dst: u32, delay: usize) -> f64 {
        let delay_s = delay as f64 / src as f64;
        let skip = (2.0 * delay_s * dst as f64) as usize + 8;
        let ideal =
            |n: usize| amplitude * (2.0 * PI * freq * (n as f64 / dst as f64 - delay_s)).sin();
        let error = rms(out
            .iter()
            .enumerate()
            .skip(skip)
            .map(|(n, &y)| y as f64 - ideal(n)));
        let signal = rms((skip..out.len()).map(ideal));
        20.0 * (signal / error).log10()
    }

    #[test]
    fn test_resampler_create() {
        let resampler = Resampler::new(8000, 16000, 1).unwrap();
        assert_eq!(resampler.src_rate(), 8000);
        assert_eq!(resampler.dst_rate(), 16000);
        assert_eq!(resampler.channels(), 1);
        assert!(resampler.needs_resampling());
        assert!(Resampler::new(0, 8000, 1).is_err());
        assert!(Resampler::new(8000, 16000, 3).is_err());
    }

    #[test]
    fn test_resampler_no_resampling() {
        let mut resampler = Resampler::new(48000, 48000, 1).unwrap();
        assert!(!resampler.needs_resampling());
        assert_eq!(resampler.delay_frames(), 0);

        let input = vec![100i16, 200, 300, 400];
        let output = resampler.resample(&input).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn a_frame_in_gives_the_frame_out_at_an_integer_ratio() {
        // 20 ms frames, as a call hands them over.
        let mut up = Resampler::new(8000, 16000, 1).unwrap();
        let mut down = Resampler::new(48000, 8000, 1).unwrap();
        for _ in 0..10 {
            assert_eq!(up.resample(&[1000; 160]).unwrap().len(), 320);
            assert_eq!(down.resample(&[1000; 960]).unwrap().len(), 160);
        }
    }

    #[test]
    fn audio_handed_over_in_frames_is_resampled_as_if_whole() {
        // The linear interpolation this replaced stepped at every frame:
        // a buzz at 50 Hz under a 20 ms-framed stream.
        for (src, dst) in [(8000, 16000), (16000, 8000), (22050, 8000), (8000, 48000)] {
            let input = tone(440.0, src, 0.5, 8000.0);
            let whole = Resampler::new(src, dst, 1)
                .unwrap()
                .resample(&input)
                .unwrap();
            let mut framed = Resampler::new(src, dst, 1).unwrap();
            let chunked: Vec<i16> = input
                .chunks(src as usize / 50)
                .flat_map(|c| framed.resample(c).unwrap())
                .collect();
            assert_eq!(whole, chunked, "{src} -> {dst}");
        }
    }

    #[test]
    fn a_tone_upsampled_is_clean() {
        let resampler = Resampler::new(8000, 16000, 1).unwrap();
        let delay = resampler.delay_frames();
        let mut resampler = resampler;
        let input = tone(1000.0, 8000, 1.0, 10000.0);
        let out: Vec<i16> = input
            .chunks(160)
            .flat_map(|c| resampler.resample(c).unwrap())
            .collect();
        let snr = snr_db(&out, 1000.0, 10000.0, 8000, 16000, delay);
        assert!(snr > 60.0, "8 kHz -> 16 kHz, 1 kHz tone: {snr:.1} dB");
    }

    #[test]
    fn a_tone_downsampled_is_clean() {
        for (src, dst) in [(16000, 8000), (48000, 8000), (22050, 8000), (48000, 16000)] {
            let resampler = Resampler::new(src, dst, 1).unwrap();
            let delay = resampler.delay_frames();
            let mut resampler = resampler;
            let out = resampler
                .resample(&tone(1000.0, src, 1.0, 10000.0))
                .unwrap();
            let snr = snr_db(&out, 1000.0, 10000.0, src, dst, delay);
            assert!(snr > 55.0, "{src} -> {dst}, 1 kHz tone: {snr:.1} dB");
        }
    }

    #[test]
    fn what_the_lower_rate_cannot_carry_is_removed() {
        // 6 kHz at 16 kHz has no place at 8 kHz: a resampler that passed it
        // would fold it down to 2 kHz.
        let mut resampler = Resampler::new(16000, 8000, 1).unwrap();
        let out = resampler
            .resample(&tone(6000.0, 16000, 1.0, 10000.0))
            .unwrap();
        let level = rms(out.iter().skip(200).map(|&y| y as f64));
        let input_level = 10000.0 / 2f64.sqrt();
        let db = 20.0 * (level / input_level).log10();
        assert!(db < -50.0, "an alias at {db:.1} dB");
    }

    #[test]
    fn a_level_is_kept() {
        let mut resampler = Resampler::new(8000, 48000, 1).unwrap();
        let skip = 2 * resampler.delay_frames() * 6;
        let output = resampler.resample(&[1000i16; 800]).unwrap();
        assert_eq!(output.len(), 4800);
        for &sample in output.iter().skip(skip) {
            assert!((sample - 1000).abs() <= 1, "{sample}");
        }
    }

    #[test]
    fn test_resampler_stereo() {
        let mut resampler = Resampler::new(8000, 16000, 2).unwrap();
        let left = tone(500.0, 8000, 0.2, 8000.0);
        let input: Vec<i16> = left.iter().flat_map(|&l| [l, -l]).collect();
        let output = resampler.resample(&input).unwrap();
        assert_eq!(output.len(), 2 * 2 * left.len());
        for pair in output.chunks(2) {
            assert!((pair[0] as i32 + pair[1] as i32).abs() <= 1, "{pair:?}");
        }
    }

    #[test]
    fn test_resampler_reset() {
        let mut resampler = Resampler::new(8000, 16000, 1).unwrap();
        let first = resampler
            .resample(&tone(1000.0, 8000, 0.1, 8000.0))
            .unwrap();
        resampler.reset();
        let again = resampler
            .resample(&tone(1000.0, 8000, 0.1, 8000.0))
            .unwrap();
        assert_eq!(first, again, "a reset starts a new stream");
    }

    #[test]
    fn a_whole_buffer_comes_out_aligned_and_complete() {
        for (src, dst) in [(22050, 8000), (8000, 16000), (48000, 8000), (16000, 8000)] {
            let input = tone(500.0, src, 0.3, 8000.0);
            let out = resample_all(&input, src, dst, 1).unwrap();
            let want = calculate_output_length(input.len(), src, dst, 1);
            assert!(
                out.len().abs_diff(want) <= 1,
                "{src} -> {dst}: {} vs {want}",
                out.len()
            );
            // No delay: the tone starts where it started.
            let snr = snr_db(&out, 500.0, 8000.0, src, dst, 0);
            assert!(snr > 50.0, "{src} -> {dst}: {snr:.1} dB");
        }
    }

    #[test]
    fn test_calculate_output_length() {
        assert_eq!(calculate_output_length(160, 8000, 16000, 1), 320);
        assert_eq!(calculate_output_length(960, 48000, 8000, 1), 160);
        assert_eq!(calculate_output_length(320, 8000, 16000, 2), 640);
    }
}
