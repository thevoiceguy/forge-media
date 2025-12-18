//! G.722 codec implementation
//!
//! G.722 is a wideband audio codec standardized by ITU-T that provides
//! 7 kHz audio bandwidth at 64, 56, or 48 kbit/s.
//!
//! This is a pure Rust implementation based on the ITU-T G.722 specification.
//!
//! Key features:
//! - Sample rate: 16 kHz (wideband)
//! - Bit rate: 64 kbit/s (default), 56 kbit/s, or 48 kbit/s
//! - Uses Sub-Band ADPCM (SB-ADPCM) coding
//! - Splits audio into two sub-bands via QMF (Quadrature Mirror Filter)
//!   - Lower band: 0-4 kHz (6 bits for 64k mode)
//!   - Upper band: 4-8 kHz (2 bits for 64k mode)

use crate::AudioCodec;
use crate::{AudioCodecType, AudioFormat, CodecError, Result};

/// G.722 bit rates
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum G722BitRate {
    /// 64 kbit/s (most common) - 6 bits lower, 2 bits upper
    Rate64k,
    /// 56 kbit/s - 5 bits lower, 2 bits upper
    Rate56k,
    /// 48 kbit/s - 4 bits lower, 2 bits upper
    Rate48k,
}

impl G722BitRate {
    /// Get bits for lower sub-band
    pub fn lower_bits(&self) -> usize {
        match self {
            G722BitRate::Rate64k => 6,
            G722BitRate::Rate56k => 5,
            G722BitRate::Rate48k => 4,
        }
    }

    /// Get bits for upper sub-band (always 2 bits)
    pub fn upper_bits(&self) -> usize {
        2
    }

    /// Number of auxiliary bits carried in each octet (bit-stealing for 56k/48k)
    pub fn aux_bits(&self) -> usize {
        match self {
            G722BitRate::Rate64k => 0,
            G722BitRate::Rate56k => 1,
            G722BitRate::Rate48k => 2,
        }
    }
}

/// QMF filter coefficients for analysis and synthesis
/// These are the standard ITU-T G.722 coefficients
const QMF_COEFFS: [i32; 24] = [
    3, -11, -11, 53, 12, -156, 32, 362, -210, -805, 951, 3876, 3876, 951, -805, -210, 362, 32,
    -156, 12, 53, -11, -11, 3,
];

/// ITU-T G.722 quantizer magnitude table (2-bit, upper sub-band)
const QM2: [i32; 4] = [-7408, -1616, 7408, 1616];

/// ITU-T G.722 quantizer magnitude table (4-bit, lower sub-band - 48k mode)
const QM4: [i32; 16] = [
    0, -20456, -12896, -8968, -6288, -4240, -2584, -1200, 20456, 12896, 8968, 6288, 4240, 2584,
    1200, 0,
];

/// ITU-T G.722 quantizer magnitude table (5-bit, lower sub-band - 56k mode)
const QM5: [i32; 32] = [
    -280, -280, -23352, -17560, -14120, -11664, -9752, -8184, -6864, -5712, -4696, -3784, -2960,
    -2208, -1520, -880, 23352, 17560, 14120, 11664, 9752, 8184, 6864, 5712, 4696, 3784, 2960, 2208,
    1520, 880, 280, -280,
];

/// ITU-T G.722 quantizer magnitude table (6-bit, lower sub-band - 64k mode)
const QM6: [i32; 64] = [
    -136, -136, -136, -136, -24808, -21904, -19008, -16704, -14984, -13512, -12280, -11192, -10232,
    -9360, -8576, -7856, -7192, -6576, -6000, -5456, -4944, -4464, -4008, -3576, -3168, -2776,
    -2400, -2032, -1688, -1360, -1040, -728, 24808, 21904, 19008, 16704, 14984, 13512, 12280,
    11192, 10232, 9360, 8576, 7856, 7192, 6576, 6000, 5456, 4944, 4464, 4008, 3576, 3168, 2776,
    2400, 2032, 1688, 1360, 1040, 728, 432, 136, -432, -136,
];

/// ITU-T G.722 inverse log buffer (scale factor adaptation)
const ILB: [i32; 32] = [
    2048, 2093, 2139, 2186, 2233, 2282, 2332, 2383, 2435, 2489, 2543, 2599, 2656, 2714, 2774, 2834,
    2896, 2960, 3025, 3091, 3158, 3228, 3298, 3371, 3444, 3520, 3597, 3676, 3756, 3838, 3922, 4008,
];

/// ITU-T G.722 quantization index mapping for lower band (maps to wl table)
const RL42_6: [usize; 64] = [
    0, 7, 6, 5, 4, 3, 2, 1, 7, 6, 5, 4, 3, 2, 1, 0, 0, 7, 6, 5, 4, 3, 2, 1, 7, 6, 5, 4, 3, 2, 1, 0,
    0, 7, 6, 5, 4, 3, 2, 1, 7, 6, 5, 4, 3, 2, 1, 0, 0, 7, 6, 5, 4, 3, 2, 1, 7, 6, 5, 4, 3, 2, 1, 0,
];

const RL42_5: [usize; 32] = [
    0, 7, 6, 5, 4, 3, 2, 1, 7, 6, 5, 4, 3, 2, 1, 0, 0, 7, 6, 5, 4, 3, 2, 1, 7, 6, 5, 4, 3, 2, 1, 0,
];

const RL42_4: [usize; 16] = [0, 7, 6, 5, 4, 3, 2, 1, 7, 6, 5, 4, 3, 2, 1, 0];

/// ITU-T G.722 quantization index mapping for upper band (maps to wh table)
const RH2: [usize; 4] = [0, 2, 1, 2];

/// ITU-T G.722 scale factor adaptation table (lower band)
const WL: [i32; 8] = [-60, -30, 58, 172, 334, 538, 1198, 3042];

/// ITU-T G.722 scale factor adaptation table (upper band)
const WH: [i32; 3] = [0, -214, 798];

/// ITU-T G.722 decision thresholds for 6-bit quantizer (lower band, 64k mode)
/// These are compared against the input magnitude to determine quantization index
const Q6: [i32; 32] = [
    0, 35, 72, 110, 150, 190, 233, 276, 323, 370, 422, 473, 530, 587, 650, 714, 786, 858, 940,
    1023, 1121, 1219, 1339, 1458, 1612, 1765, 1980, 2195, 2557, 2919, 0, 0,
];

/// ITU-T G.722 Gray code mapping for lower band (negative values)
/// Maps threshold index to Gray-coded quantizer output
const ILN: [i32; 32] = [
    0, 63, 62, 31, 30, 29, 28, 27, 26, 25, 24, 23, 22, 21, 20, 19, 18, 17, 16, 15, 14, 13, 12, 11,
    10, 9, 8, 7, 6, 5, 4, 0,
];

/// ITU-T G.722 Gray code mapping for lower band (positive values)
const ILP: [i32; 32] = [
    0, 61, 60, 59, 58, 57, 56, 55, 54, 53, 52, 51, 50, 49, 48, 47, 46, 45, 44, 43, 42, 41, 40, 39,
    38, 37, 36, 35, 34, 33, 32, 0,
];

/// ITU-T G.722 Gray code mapping for upper band (negative values)
const IHN: [i32; 3] = [0, 1, 0];

/// ITU-T G.722 Gray code mapping for upper band (positive values)
const IHP: [i32; 3] = [0, 3, 2];

/// G.722 encoder state
pub struct G722Encoder {
    /// QMF filter history for analysis
    x: [i32; 24],
    /// Lower band ADPCM state
    band_lower: AdpcmBand,
    /// Upper band ADPCM state
    band_upper: AdpcmBand,
    /// Bit rate mode
    mode: G722BitRate,
}

/// G.722 decoder state
pub struct G722Decoder {
    /// QMF filter history for synthesis
    r: [i32; 24],
    /// Lower band ADPCM state
    band_lower: AdpcmBand,
    /// Upper band ADPCM state
    band_upper: AdpcmBand,
    /// Bit rate mode
    mode: G722BitRate,
}

/// ADPCM sub-band state
#[derive(Clone)]
struct AdpcmBand {
    /// Predictor state (signal estimate)
    s: i32,
    /// Predictor coefficients (2nd order poles)
    a: [i32; 2],
    /// Predictor coefficients (6th order zeros)
    b: [i32; 6],
    /// Signal estimate from zeros (FILTEZ output)
    sz: i32,
    /// Signal estimate from poles (FILTD output)
    sp: i32,
    /// Quantizer scale factor
    det: i32,
    /// Noise estimate (for scale factor adaptation)
    nb: i32,
    /// Delay line for quantized differences (FILTEZ input)
    dq: [i32; 7],
    /// Delay line for reconstructed signals (FILTD input)
    r: [i32; 3],
}

impl AdpcmBand {
    fn new() -> Self {
        // Initial nb = 8320 gives det ≈ 544 via SCALEL formula
        // This matches common G.722 implementations for good initial adaptation
        let nb = 8320;
        let wd1 = (nb >> 6) & 31; // = 2
        let wd2 = 8 - (nb >> 11); // = 4
        let wd3 = ILB[wd1 as usize] >> wd2; // ILB[2] >> 4 = 2139 >> 4 = 133
        let det = (wd3 << 2).clamp(0, 32767); // = 532

        Self {
            s: 0,
            a: [0; 2],
            b: [0; 6],
            sz: 0,
            sp: 0,
            det,        // Computed from nb using SCALEL formula
            nb,         // Initial noise estimate
            dq: [0; 7], // Quantized difference delay line (FILTEZ)
            r: [0; 3],  // Reconstructed signal delay line (FILTD)
        }
    }

    /// Saturating 16-bit add
    fn sat_add16(a: i32, b: i32) -> i32 {
        let result = (a as i64 + b as i64).clamp(i16::MIN as i64, i16::MAX as i64);
        result as i32
    }

    /// Saturating 16-bit subtract
    fn sat_sub16(a: i32, b: i32) -> i32 {
        let result = (a as i64 - b as i64).clamp(i16::MIN as i64, i16::MAX as i64);
        result as i32
    }

    /// Saturate to 16-bit range
    fn saturate16(val: i32) -> i32 {
        val.clamp(i16::MIN as i32, i16::MAX as i32)
    }

    /// Update predictor after encoding/decoding a sample (ITU-T G.722 algorithm)
    /// ril: quantization index for LOGSCL adaptation
    ///
    /// Uses ITU-T FILTEZ and FILTD (UPPOL) as specified:
    /// - FILTEZ: Zero predictor using dq[] delay line (quantized differences)
    /// - FILTD: Pole predictor using r[] delay line (reconstructed signals)
    fn update(&mut self, dq: i32, r: i32, ril: usize, is_lower: bool) {
        // UPZERO + FILTEZ: Update b coefficients (6th order zeros)
        // Uses dq[] delay line as per ITU-T G.722 specification
        let wd1 = if dq == 0 { 0 } else { 128 };
        let mut sz = 0i64;
        for i in (0..6).rev() {
            // Update coefficient using delayed quantized difference
            let wd2 = if (self.dq[i + 1] ^ dq) & 0x8000 != 0 {
                -wd1
            } else {
                wd1
            };
            let wd3 = (self.b[i] as i64 * 32640) >> 15;
            self.b[i] = Self::sat_add16(wd2, wd3 as i32);

            // FILTEZ: Compute zero predictor using dq delay line
            let wd3 = Self::sat_add16(self.dq[i], self.dq[i]);
            sz += (self.b[i] as i64 * wd3 as i64) >> 15;

            // Shift dq delay line
            if i < 6 {
                self.dq[i + 1] = self.dq[i];
            }
        }
        self.dq[0] = dq; // Store new quantized difference
        self.sz = Self::saturate16(sz as i32);

        // UPPOL2: Update a[1] (2nd order pole)
        // Uses r[] delay line as per ITU-T G.722 specification
        let wd1 = Self::saturate16(self.a[0] << 2);
        let wd32 = if (r ^ self.r[0]) & 0x8000 != 0 {
            wd1
        } else {
            -wd1
        };
        let wd32 = wd32.clamp(-32767, 32767);
        let wd3 = {
            let term1 = if (r ^ self.r[1]) & 0x8000 != 0 {
                -128
            } else {
                128
            };
            let term2 = wd32 >> 7;
            let term3 = (self.a[1] as i64 * 32512) >> 15;
            term1 + term2 + term3 as i32
        };
        let ap1 = wd3.clamp(-12288, 12288);

        // UPPOL1: Update a[0] (1st order pole)
        let wd1 = if (r ^ self.r[0]) & 0x8000 != 0 {
            -192
        } else {
            192
        };
        let wd2 = (self.a[0] as i64 * 32640) >> 15;
        let mut ap0 = Self::sat_add16(wd1, wd2 as i32);

        // Limiter: Constrain a[0] based on a[1]
        let wd3 = Self::sat_sub16(15360, ap1);
        if ap0.abs() > wd3 {
            ap0 = if ap0 < 0 { -wd3 } else { wd3 };
        }

        // Apply updated coefficients
        self.a[0] = ap0;
        self.a[1] = ap1;

        // FILTD: Compute pole predictor using r[] delay line
        let mut sp = 0i64;
        sp += (self.a[0] as i64 * self.r[0] as i64) >> 15;
        sp += (self.a[1] as i64 * self.r[1] as i64) >> 15;
        self.sp = Self::saturate16(sp as i32);

        // Shift r[] delay line AFTER using old values
        self.r[2] = self.r[1];
        self.r[1] = self.r[0];
        self.r[0] = r;

        // Total signal estimate: s = sz + sp
        self.s = self.sp.saturating_add(self.sz);

        // LOGSCL + SCALEL: Scale factor adaptation using ITU tables
        let wd1 = (self.nb * 127) >> 7;
        let wl_or_wh = if is_lower { WL[ril] } else { WH[ril] };
        let mut wd1 = wd1 + wl_or_wh;
        wd1 = wd1.clamp(0, 18432);
        self.nb = wd1;

        // SCALEL: Convert nb to det using ILB table
        let wd1 = (self.nb >> 6) & 31;
        let wd2 = 8 - (self.nb >> 11);
        let wd3 = if wd2 < 0 {
            ILB[wd1 as usize] << (-wd2)
        } else {
            ILB[wd1 as usize] >> wd2
        };
        self.det = (wd3 << 2).clamp(0, 32767);
    }
}

impl G722Encoder {
    /// Create a new G.722 encoder
    pub fn new(mode: G722BitRate) -> Self {
        Self {
            x: [0; 24],
            band_lower: AdpcmBand::new(),
            band_upper: AdpcmBand::new(),
            mode,
        }
    }

    /// Encode PCM16 samples to G.722
    ///
    /// Input: 16-bit PCM samples at 16 kHz (must be even length)
    /// Output: Encoded bytes
    ///   - 64k mode: 8 bits per frame (2 samples) = 1 byte per frame
    ///   - 56k mode: 7 bits per frame, packed across byte boundaries
    ///   - 48k mode: 6 bits per frame, packed across byte boundaries
    ///
    /// Returns error if input length is odd (G.722 requires pairs of samples).
    pub fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        Ok(self.encode_with_aux(samples, &[])?.0)
    }

    /// Encode PCM16 samples to G.722 while embedding auxiliary bits (56k/48k modes).
    ///
    /// The returned tuple is (encoded_octets, aux_bits_consumed).
    pub fn encode_with_aux(
        &mut self,
        samples: &[i16],
        aux_bits: &[u8],
    ) -> Result<(Vec<u8>, usize)> {
        // G.722 encodes 2 samples at a time, input must be even length
        if samples.len() % 2 != 0 {
            return Err(CodecError::Encoding(format!(
                "G.722 requires even number of samples, got {}",
                samples.len()
            )));
        }

        let mut output = Vec::with_capacity(samples.len() / 2);

        // ITU-T G.722 always outputs 8-bit octets regardless of bit rate
        // Bit layout (MSB first):
        //   64k: [upper 2][lower 6][aux 0] = 8 bits
        //   56k: [upper 2][lower 5][aux 1] = 8 bits
        //   48k: [upper 2][lower 4][aux 2] = 8 bits
        // Aux bits are reserved/unused (set to 0)

        let lower_bits = self.mode.lower_bits();
        let aux_bits_per_frame = self.mode.aux_bits();
        let mut aux_idx = 0usize;

        for chunk in samples.chunks_exact(2) {
            // QMF analysis - split into lower and upper bands
            let (xlow, xhigh) = self.qmf_analysis(chunk[0], chunk[1]);

            // Encode lower and upper bands (returns Gray-coded indices)
            let ilow = self.encode_lower(xlow) as u8;
            let ihigh = self.encode_upper(xhigh) as u8;

            // Pack into 8-bit octet: upper band in bits 7-6, lower band in bits 5-0 (or fewer)
            // Mask lower band to actual bit width, aux bits implicitly 0
            let lower_mask = ((1 << lower_bits) - 1) as u8;

            let aux_val: u8 = if aux_bits_per_frame == 0 {
                0
            } else {
                let mut v = 0u8;
                for b in 0..aux_bits_per_frame {
                    let bit = *aux_bits.get(aux_idx + b).unwrap_or(&0) & 0x01;
                    v |= bit << b;
                }
                v
            };
            aux_idx = aux_idx.saturating_add(aux_bits_per_frame);

            let octet = (ihigh << 6) | ((ilow & lower_mask) << aux_bits_per_frame as u8) | aux_val;

            output.push(octet);
        }

        Ok((output, aux_idx))
    }

    /// QMF analysis filter - split signal into two sub-bands
    /// Uses ITU-T G.722 symmetric tap algorithm with >>14 normalization
    fn qmf_analysis(&mut self, sample0: i16, sample1: i16) -> (i32, i32) {
        // Shift delay line
        for i in (2..24).rev() {
            self.x[i] = self.x[i - 2];
        }
        self.x[0] = sample0 as i32;
        self.x[1] = sample1 as i32;

        // Apply QMF filter using symmetric taps
        // ITU standard: combine x[i] and x[23-i] with >>14 shift
        let mut xlow = 0i32;
        let mut xhigh = 0i32;

        for i in 0..12 {
            let tap_sum = self.x[i].saturating_add(self.x[23 - i]);
            let tap_diff = self.x[i].saturating_sub(self.x[23 - i]);
            xlow = xlow.saturating_add(((tap_sum as i64 * QMF_COEFFS[i] as i64) >> 14) as i32);
            xhigh = xhigh.saturating_add(((tap_diff as i64 * QMF_COEFFS[i] as i64) >> 14) as i32);
        }

        (xlow, xhigh)
    }

    /// Encode lower sub-band sample using ITU-T threshold-based quantization
    fn encode_lower(&mut self, xlow: i32) -> i32 {
        // Compute difference signal
        let el = xlow.saturating_sub(self.band_lower.s);

        // Get magnitude of difference
        let wd = el.saturating_abs();

        // Find quantization index using ITU decision thresholds
        // Note: For all modes we search up to 30 thresholds in Q6
        // The quantization is determined by which threshold is crossed
        let mut i = 1;
        while i < 30 {
            let wd1 = ((Q6[i] as i64 * self.band_lower.det as i64) >> 12) as i32;
            if wd < wd1 {
                break;
            }
            i += 1;
        }

        // Apply Gray coding based on signal polarity
        let ilow = if el < 0 { ILN[i] } else { ILP[i] };

        // For lower bit rates, map 6-bit index to appropriate range
        // by taking the upper bits (the quantization is coarser)
        let (ilow_adjusted, qm_table): (usize, &[i32]) = match self.mode {
            G722BitRate::Rate64k => (ilow as usize, &QM6[..]),
            G722BitRate::Rate56k => ((ilow >> 1) as usize, &QM5[..]), // Use upper 5 bits
            G722BitRate::Rate48k => ((ilow >> 2) as usize, &QM4[..]), // Use upper 4 bits
        };

        // Reconstruct using quantizer magnitude table
        let dq = ((qm_table[ilow_adjusted] as i64 * self.band_lower.det as i64) >> 15) as i32;
        let rlow = self.band_lower.s.saturating_add(dq);

        // Map to adaptation index
        let ril = match self.mode {
            G722BitRate::Rate64k => RL42_6[ilow_adjusted],
            G722BitRate::Rate56k => RL42_5[ilow_adjusted],
            G722BitRate::Rate48k => RL42_4[ilow_adjusted],
        };

        // Update ADPCM state
        self.band_lower.update(dq, rlow, ril, true);

        ilow_adjusted as i32
    }

    /// Encode upper sub-band sample using ITU-T threshold-based quantization
    fn encode_upper(&mut self, xhigh: i32) -> i32 {
        // Compute difference signal
        let eh = xhigh.saturating_sub(self.band_upper.s);

        // Get magnitude of difference
        let wd = eh.saturating_abs();

        // ITU-T upper band quantizer: single threshold at 564*det>>12
        let wd1 = ((564i64 * self.band_upper.det as i64) >> 12) as i32;
        let mih = if wd >= wd1 { 2 } else { 1 };

        // Apply Gray coding based on signal polarity
        let ihigh = if eh < 0 { IHN[mih] } else { IHP[mih] };

        // Reconstruct using QM2 table
        let dq = ((QM2[ihigh as usize] as i64 * self.band_upper.det as i64) >> 15) as i32;
        let rhigh = self.band_upper.s.saturating_add(dq);

        // Map to adaptation index
        let rih = RH2[ihigh as usize];

        // Update ADPCM state
        self.band_upper.update(dq, rhigh, rih, false);

        ihigh
    }
}

impl G722Decoder {
    /// Create a new G.722 decoder
    pub fn new(mode: G722BitRate) -> Self {
        Self {
            r: [0; 24],
            band_lower: AdpcmBand::new(),
            band_upper: AdpcmBand::new(),
            mode,
        }
    }

    /// Decode G.722 to PCM16 samples
    ///
    /// Input: Encoded bytes (one 8-bit octet per frame)
    /// Output: 16-bit PCM samples at 16 kHz (2 samples per octet)
    ///
    /// ITU-T G.722 octet format (MSB first):
    ///   64k: [upper 2][lower 6][aux 0]
    ///   56k: [upper 2][lower 5][aux 1]
    ///   48k: [upper 2][lower 4][aux 2]
    pub fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        self.decode_with_aux(data).0
    }

    /// Decode and extract auxiliary bits.
    /// Returns (pcm_samples, aux_bits).
    pub fn decode_with_aux(&mut self, data: &[u8]) -> (Vec<i16>, Vec<u8>) {
        let mut output = Vec::with_capacity(data.len() * 2);
        let mut aux_bits_out = Vec::with_capacity(data.len() * self.mode.aux_bits());

        let lower_bits = self.mode.lower_bits();
        let aux_bits_per_frame = self.mode.aux_bits();

        for &octet in data {
            // Extract aux bits (LSBs)
            if aux_bits_per_frame > 0 {
                let aux_mask = (1 << aux_bits_per_frame) - 1;
                let aux = octet & aux_mask;
                for b in 0..aux_bits_per_frame {
                    aux_bits_out.push((aux >> b) & 0x01);
                }
            }

            // Extract upper band (bits 7-6)
            let ihigh = (octet >> 6) as i32;

            // Extract lower band (masked to actual width, above aux bits)
            let ilow = ((octet >> aux_bits_per_frame) & ((1 << lower_bits) - 1)) as i32;

            // Decode lower band
            let rlow = self.decode_lower(ilow);

            // Decode upper band
            let rhigh = self.decode_upper(ihigh);

            // QMF synthesis - combine bands into two PCM samples
            let (sample0, sample1) = self.qmf_synthesis(rlow, rhigh);
            output.push(sample0);
            output.push(sample1);
        }

        (output, aux_bits_out)
    }

    /// Decode lower sub-band sample
    fn decode_lower(&mut self, ilow: i32) -> i32 {
        // Select quantizer table based on bit rate mode
        let nbits = self.mode.lower_bits();
        let index = (ilow as usize) & ((1 << nbits) - 1);

        // Use appropriate ITU quantizer table
        let qm_table = match self.mode {
            G722BitRate::Rate64k => &QM6[..],
            G722BitRate::Rate56k => &QM5[..],
            G722BitRate::Rate48k => &QM4[..],
        };

        let dq = ((qm_table[index] as i64 * self.band_lower.det as i64) >> 15) as i32;

        // Reconstruct
        let rlow = self.band_lower.s.saturating_add(dq);

        // Map quantization index for adaptation
        let ril = match self.mode {
            G722BitRate::Rate64k => RL42_6[index],
            G722BitRate::Rate56k => RL42_5[index],
            G722BitRate::Rate48k => RL42_4[index],
        };

        // Update state with ITU adaptation
        self.band_lower.update(dq, rlow, ril, true);

        rlow
    }

    /// Decode upper sub-band sample
    fn decode_upper(&mut self, ihigh: i32) -> i32 {
        // Inverse quantize using ITU QM2 table (2 bits, all modes)
        let index = (ihigh as usize) & 0x03;
        let dq = ((QM2[index] as i64 * self.band_upper.det as i64) >> 15) as i32;

        // Reconstruct
        let rhigh = self.band_upper.s.saturating_add(dq);

        // Map quantization index for adaptation
        let rih = RH2[index];

        // Update state with ITU adaptation
        self.band_upper.update(dq, rhigh, rih, false);

        rhigh
    }

    /// QMF synthesis filter - combine sub-bands
    /// Uses ITU-T G.722 symmetric tap algorithm with >>14 normalization
    fn qmf_synthesis(&mut self, rlow: i32, rhigh: i32) -> (i16, i16) {
        // Shift delay line
        for i in (2..24).rev() {
            self.r[i] = self.r[i - 2];
        }

        // Insert sub-band samples
        self.r[0] = rlow.saturating_add(rhigh);
        self.r[1] = rlow.saturating_sub(rhigh);

        // Apply QMF synthesis filter using symmetric taps
        // ITU standard: combine r[i] and r[23-i] with >>14 shift
        let mut sample0 = 0i32;
        let mut sample1 = 0i32;

        for i in 0..12 {
            let tap0_sum = self.r[i].saturating_add(self.r[23 - i]);
            let tap1_sum = self.r[i].saturating_sub(self.r[23 - i]);
            sample0 =
                sample0.saturating_add(((tap0_sum as i64 * QMF_COEFFS[i] as i64) >> 14) as i32);
            sample1 =
                sample1.saturating_add(((tap1_sum as i64 * QMF_COEFFS[i] as i64) >> 14) as i32);
        }

        (
            sample0.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            sample1.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        )
    }
}

/// G.722 codec wrapper implementing AudioCodec trait
pub struct G722Codec {
    encoder: G722Encoder,
    decoder: G722Decoder,
    bit_rate: G722BitRate,
}

impl G722Codec {
    /// Create a new G.722 codec
    pub fn new(bit_rate: G722BitRate) -> Self {
        Self {
            encoder: G722Encoder::new(bit_rate),
            decoder: G722Decoder::new(bit_rate),
            bit_rate,
        }
    }

    /// Get the bit rate
    pub fn bit_rate(&self) -> G722BitRate {
        self.bit_rate
    }
}

impl Default for G722Codec {
    fn default() -> Self {
        Self::new(G722BitRate::Rate64k)
    }
}

impl AudioCodec for G722Codec {
    fn name(&self) -> &str {
        "G.722"
    }

    fn native_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: 16000,
            channels: 1,
            codec: AudioCodecType::G722,
        }
    }

    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        self.encoder.encode(samples)
    }

    fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>> {
        Ok(self.decoder.decode(data))
    }

    fn reset(&mut self) {
        self.encoder = G722Encoder::new(self.bit_rate);
        self.decoder = G722Decoder::new(self.bit_rate);
    }

    fn frame_size(&self) -> Option<usize> {
        // G.722 typically processes 10ms frames at 16kHz = 160 samples
        Some(160)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_g722_codec_creation() {
        let codec = G722Codec::new(G722BitRate::Rate64k);
        assert_eq!(codec.bit_rate(), G722BitRate::Rate64k);
        assert_eq!(codec.name(), "G.722");
    }

    #[test]
    fn test_g722_encode_decode_silence() {
        let mut codec = G722Codec::default();

        // Encode 10ms of silence (160 samples @ 16kHz)
        let silence: Vec<i16> = vec![0; 160];
        let encoded = codec.encode(&silence).expect("Encoding failed");
        assert_eq!(encoded.len(), 80); // 2 samples per byte

        // Decode back
        let decoded = codec.decode(&encoded).expect("Decoding failed");
        assert_eq!(decoded.len(), 160);

        // Silence should decode to near-silence
        // ITU-T G.722 ADPCM has inherent quantization noise, allow up to 500 (~1.5% of max)
        let max_sample = decoded
            .iter()
            .map(|&x| x.saturating_abs())
            .max()
            .unwrap_or(0);
        assert!(max_sample < 500, "Decoded silence too loud: {}", max_sample);
    }

    #[test]
    fn test_g722_encode_decode_tone() {
        let mut codec = G722Codec::new(G722BitRate::Rate64k);

        // Generate multiple frames of a 1kHz tone to allow predictor warmup
        // 320 samples = 20ms @ 16kHz (2 frames)
        let mut samples = Vec::with_capacity(320);
        for i in 0..320 {
            let phase = (i as f64 * 2.0 * std::f64::consts::PI * 1000.0 / 16000.0);
            let sample = (10000.0 * phase.sin()) as i16;
            samples.push(sample);
        }

        // Encode
        let encoded = codec.encode(&samples).expect("Encoding failed");
        assert_eq!(encoded.len(), 160); // 2 samples per byte

        // Decode
        let decoded = codec.decode(&encoded).expect("Decoding failed");
        assert_eq!(decoded.len(), 320);

        // Check that decoded signal is not all zeros
        // Skip first 80 samples to allow predictor warmup
        let tail_samples = &decoded[80..];
        let energy: i64 = tail_samples.iter().map(|&x| (x as i64).pow(2)).sum();
        let max_amplitude = tail_samples
            .iter()
            .map(|&x| x.saturating_abs())
            .max()
            .unwrap_or(0);

        // G.722 ADPCM should preserve some signal (not perfect, but not silence)
        assert!(
            energy > 10,
            "Decoded signal is complete silence: {}",
            energy
        );
        assert!(
            max_amplitude > 10,
            "Decoded amplitude too small: {}",
            max_amplitude
        );

        // Also verify encoding compresses the data
        assert!(encoded.len() < samples.len() * 2);
    }

    #[test]
    fn test_g722_different_bit_rates() {
        for bit_rate in [
            G722BitRate::Rate64k,
            G722BitRate::Rate56k,
            G722BitRate::Rate48k,
        ] {
            let mut codec = G722Codec::new(bit_rate);
            let samples: Vec<i16> = vec![500; 160];

            let encoded = codec.encode(&samples).expect("Encoding failed");
            let decoded = codec.decode(&encoded).expect("Decoding failed");

            assert_eq!(decoded.len(), 160);
        }
    }

    #[test]
    fn test_g722_reset() {
        let mut codec = G722Codec::default();

        // Encode some data
        let samples: Vec<i16> = vec![1000; 160];
        let _ = codec.encode(&samples);

        // Reset
        codec.reset();

        // Encode silence after reset
        let silence: Vec<i16> = vec![0; 160];
        let encoded = codec.encode(&silence).expect("Encoding after reset failed");
        assert_eq!(encoded.len(), 80);
    }

    #[test]
    fn test_g722_native_format() {
        let codec = G722Codec::default();
        let format = codec.native_format();
        assert_eq!(format.sample_rate, 16000);
        assert_eq!(format.channels, 1);
        assert_eq!(format.codec, AudioCodecType::G722);
    }

    #[test]
    fn test_g722_frame_size() {
        let codec = G722Codec::default();
        assert_eq!(codec.frame_size(), Some(160));
    }

    #[test]
    fn test_g722_odd_length_input_returns_error() {
        let mut codec = G722Codec::default();
        let odd_samples: Vec<i16> = vec![0; 159]; // Odd number
        let result = codec.encode(&odd_samples);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("even number of samples"));
    }

    #[test]
    fn test_g722_bit_rate_affects_output_size() {
        // Test that lower bit rates produce smaller output
        // Generate proper sine wave: 1kHz tone at 16kHz sample rate
        let samples: Vec<i16> = (0..320)
            .map(|i| {
                let phase = (i as f64 * 2.0 * std::f64::consts::PI * 1000.0) / 16000.0;
                (phase.sin() * 10000.0) as i16
            })
            .collect();

        let mut codec_64k = G722Codec::new(G722BitRate::Rate64k);
        let mut codec_56k = G722Codec::new(G722BitRate::Rate56k);
        let mut codec_48k = G722Codec::new(G722BitRate::Rate48k);

        let encoded_64k = codec_64k.encode(&samples).expect("64k encode failed");
        let encoded_56k = codec_56k.encode(&samples).expect("56k encode failed");
        let encoded_48k = codec_48k.encode(&samples).expect("48k encode failed");

        // ITU-T G.722 always outputs 8-bit octets regardless of bit rate
        // 320 samples = 160 frames (2 samples per frame)
        // All modes: 8 bits/frame = 1280 bits = 160 bytes
        // Difference is in audio vs. aux bits:
        //   64k: 6+2 audio bits, 0 aux bits
        //   56k: 5+2 audio bits, 1 aux bit
        //   48k: 4+2 audio bits, 2 aux bits
        assert_eq!(encoded_64k.len(), 160, "64k should be 160 bytes");
        assert_eq!(
            encoded_56k.len(),
            160,
            "56k should be 160 bytes (8-bit octets with aux)"
        );
        assert_eq!(
            encoded_48k.len(),
            160,
            "48k should be 160 bytes (8-bit octets with aux)"
        );

        // Verify decode works
        let decoded_64k = codec_64k.decode(&encoded_64k).expect("64k decode failed");
        let decoded_56k = codec_56k.decode(&encoded_56k).expect("56k decode failed");
        let decoded_48k = codec_48k.decode(&encoded_48k).expect("48k decode failed");

        assert_eq!(decoded_64k.len(), 320);
        assert_eq!(decoded_56k.len(), 320);
        assert_eq!(decoded_48k.len(), 320);
    }

    #[test]
    fn test_g722_aux_bits_round_trip() {
        // 48k mode carries 2 aux bits per octet
        let bit_rate = G722BitRate::Rate48k;
        let aux_per_frame = bit_rate.aux_bits();

        let mut encoder = G722Encoder::new(bit_rate);
        let mut decoder = G722Decoder::new(bit_rate);

        let samples: Vec<i16> = vec![0; 160]; // 80 frames
        let frames = samples.len() / 2;

        let aux_bits: Vec<u8> = (0..frames * aux_per_frame)
            .map(|i| (i & 0x01) as u8)
            .collect();

        let (encoded, consumed) = encoder
            .encode_with_aux(&samples, &aux_bits)
            .expect("encode_with_aux failed");
        assert_eq!(consumed, frames * aux_per_frame);

        let (decoded, recovered_aux) = decoder.decode_with_aux(&encoded);
        assert_eq!(decoded.len(), samples.len());
        assert_eq!(
            &recovered_aux[..frames * aux_per_frame],
            &aux_bits[..frames * aux_per_frame]
        );
    }

    // TODO: Add ITU-T G.722 test vector validation
    //
    // For true interoperability verification, this implementation should be tested against
    // official ITU-T G.722 test vectors. Test vectors can be obtained from:
    //
    // 1. ITU-T Software Tool Library (STL): https://www.itu.int/rec/T-REC-G.191/
    //    - Contains reference implementations and test vectors
    //    - Vectors include various input signals and expected encoded/decoded outputs
    //
    // 2. Test vector format:
    //    - Input PCM files (16-bit linear PCM at 16 kHz)
    //    - Expected G.722 bitstream files (encoded output)
    //    - Expected decoded PCM files (for round-trip testing)
    //
    // 3. Recommended test cases:
    //    - Sine waves at various frequencies (500Hz, 1kHz, 2kHz, 3kHz)
    //    - Speech signals (male/female voices)
    //    - Composite signals
    //    - All three bit rates (64k, 56k, 48k)
    //
    // 4. Validation criteria:
    //    - Bit-exact match for encoded bitstreams
    //    - SNR > 30dB for decoded signals (allowing for codec distortion)
    //    - Cross-interop with reference encoder/decoder
    //
    // Example test structure:
    //
    // #[test]
    // fn test_g722_itu_test_vector_1() {
    //     let input_pcm = load_test_vector("itu_vectors/sine_1khz_16khz.pcm");
    //     let expected_g722 = load_test_vector("itu_vectors/sine_1khz_64k.g722");
    //
    //     let mut encoder = G722Encoder::new(G722BitRate::Rate64k);
    //     let encoded = encoder.encode(&input_pcm).unwrap();
    //
    //     // Verify bit-exact encoding
    //     assert_eq!(encoded, expected_g722, "Encoded output doesn't match ITU reference");
    //
    //     let mut decoder = G722Decoder::new(G722BitRate::Rate64k);
    //     let decoded = decoder.decode(&encoded);
    //
    //     // Verify reasonable SNR (not bit-exact due to ADPCM)
    //     let snr = calculate_snr(&input_pcm, &decoded);
    //     assert!(snr > 30.0, "SNR too low: {} dB", snr);
    // }
}
