//! G.711 codec implementation
//!
//! G.711 is a narrowband audio codec that compresses 16-bit linear PCM samples
//! to 8-bit logarithmic samples. It provides toll-quality audio at 64 kbit/s.
//!
//! Two algorithms are defined:
//! - μ-law (PCMU): Used in North America and Japan
//! - A-law (PCMA): Used in Europe and the rest of the world
//!
//! This implementation uses standard ITU-T G.711 lookup tables.

use crate::AudioCodec;
use crate::{AudioFormat, Result};

// μ-law encoding lookup table (14-bit input to 8-bit)
#[allow(dead_code)]
const ULAW_ENC_TABLE: &[u8; 256] = &[
    0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA, 0xF9, 0xF8, 0xF7, 0xF6, 0xF5, 0xF4, 0xF3, 0xF2, 0xF1, 0xF0,
    0xEF, 0xEE, 0xED, 0xEC, 0xEB, 0xEA, 0xE9, 0xE8, 0xE7, 0xE6, 0xE5, 0xE4, 0xE3, 0xE2, 0xE1, 0xE0,
    0xDF, 0xDF, 0xDE, 0xDE, 0xDD, 0xDD, 0xDC, 0xDC, 0xDB, 0xDB, 0xDA, 0xDA, 0xD9, 0xD9, 0xD8, 0xD8,
    0xD7, 0xD7, 0xD6, 0xD6, 0xD5, 0xD5, 0xD4, 0xD4, 0xD3, 0xD3, 0xD2, 0xD2, 0xD1, 0xD1, 0xD0, 0xD0,
    0xCF, 0xCF, 0xCF, 0xCF, 0xCE, 0xCE, 0xCE, 0xCE, 0xCD, 0xCD, 0xCD, 0xCD, 0xCC, 0xCC, 0xCC, 0xCC,
    0xCB, 0xCB, 0xCB, 0xCB, 0xCA, 0xCA, 0xCA, 0xCA, 0xC9, 0xC9, 0xC9, 0xC9, 0xC8, 0xC8, 0xC8, 0xC8,
    0xC7, 0xC7, 0xC7, 0xC7, 0xC7, 0xC7, 0xC7, 0xC7, 0xC6, 0xC6, 0xC6, 0xC6, 0xC6, 0xC6, 0xC6, 0xC6,
    0xC5, 0xC5, 0xC5, 0xC5, 0xC5, 0xC5, 0xC5, 0xC5, 0xC4, 0xC4, 0xC4, 0xC4, 0xC4, 0xC4, 0xC4, 0xC4,
    0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3, 0xC3,
    0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2, 0xC2,
    0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1, 0xC1,
    0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0,
    0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF,
    0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE, 0xBE,
    0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD, 0xBD,
    0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC, 0xBC,
];

// μ-law decoding lookup table (8-bit to 16-bit)
const ULAW_DEC_TABLE: &[i16; 256] = &[
    -32124, -31100, -30076, -29052, -28028, -27004, -25980, -24956, -23932, -22908, -21884, -20860,
    -19836, -18812, -17788, -16764, -15996, -15484, -14972, -14460, -13948, -13436, -12924, -12412,
    -11900, -11388, -10876, -10364, -9852, -9340, -8828, -8316, -7932, -7676, -7420, -7164, -6908,
    -6652, -6396, -6140, -5884, -5628, -5372, -5116, -4860, -4604, -4348, -4092, -3900, -3772,
    -3644, -3516, -3388, -3260, -3132, -3004, -2876, -2748, -2620, -2492, -2364, -2236, -2108,
    -1980, -1884, -1820, -1756, -1692, -1628, -1564, -1500, -1436, -1372, -1308, -1244, -1180,
    -1116, -1052, -988, -924, -876, -844, -812, -780, -748, -716, -684, -652, -620, -588, -556,
    -524, -492, -460, -428, -396, -372, -356, -340, -324, -308, -292, -276, -260, -244, -228, -212,
    -196, -180, -164, -148, -132, -120, -112, -104, -96, -88, -80, -72, -64, -56, -48, -40, -32,
    -24, -16, -8, 0, 32124, 31100, 30076, 29052, 28028, 27004, 25980, 24956, 23932, 22908, 21884,
    20860, 19836, 18812, 17788, 16764, 15996, 15484, 14972, 14460, 13948, 13436, 12924, 12412,
    11900, 11388, 10876, 10364, 9852, 9340, 8828, 8316, 7932, 7676, 7420, 7164, 6908, 6652, 6396,
    6140, 5884, 5628, 5372, 5116, 4860, 4604, 4348, 4092, 3900, 3772, 3644, 3516, 3388, 3260, 3132,
    3004, 2876, 2748, 2620, 2492, 2364, 2236, 2108, 1980, 1884, 1820, 1756, 1692, 1628, 1564, 1500,
    1436, 1372, 1308, 1244, 1180, 1116, 1052, 988, 924, 876, 844, 812, 780, 748, 716, 684, 652,
    620, 588, 556, 524, 492, 460, 428, 396, 372, 356, 340, 324, 308, 292, 276, 260, 244, 228, 212,
    196, 180, 164, 148, 132, 120, 112, 104, 96, 88, 80, 72, 64, 56, 48, 40, 32, 24, 16, 8, 0,
];

// A-law encoding lookup table (13-bit input to 8-bit)
#[allow(dead_code)]
const ALAW_ENC_TABLE: &[u8; 256] = &[
    0xD5, 0xD4, 0xD7, 0xD6, 0xD1, 0xD0, 0xD3, 0xD2, 0xDD, 0xDC, 0xDF, 0xDE, 0xD9, 0xD8, 0xDB, 0xDA,
    0xC5, 0xC4, 0xC7, 0xC6, 0xC1, 0xC0, 0xC3, 0xC2, 0xCD, 0xCC, 0xCF, 0xCE, 0xC9, 0xC8, 0xCB, 0xCA,
    0xF5, 0xF5, 0xF4, 0xF4, 0xF7, 0xF7, 0xF6, 0xF6, 0xF1, 0xF1, 0xF0, 0xF0, 0xF3, 0xF3, 0xF2, 0xF2,
    0xFD, 0xFD, 0xFC, 0xFC, 0xFF, 0xFF, 0xFE, 0xFE, 0xF9, 0xF9, 0xF8, 0xF8, 0xFB, 0xFB, 0xFA, 0xFA,
    0xE5, 0xE5, 0xE5, 0xE5, 0xE4, 0xE4, 0xE4, 0xE4, 0xE7, 0xE7, 0xE7, 0xE7, 0xE6, 0xE6, 0xE6, 0xE6,
    0xE1, 0xE1, 0xE1, 0xE1, 0xE0, 0xE0, 0xE0, 0xE0, 0xE3, 0xE3, 0xE3, 0xE3, 0xE2, 0xE2, 0xE2, 0xE2,
    0xED, 0xED, 0xED, 0xED, 0xEC, 0xEC, 0xEC, 0xEC, 0xEF, 0xEF, 0xEF, 0xEF, 0xEE, 0xEE, 0xEE, 0xEE,
    0xE9, 0xE9, 0xE9, 0xE9, 0xE8, 0xE8, 0xE8, 0xE8, 0xEB, 0xEB, 0xEB, 0xEB, 0xEA, 0xEA, 0xEA, 0xEA,
    0x95, 0x95, 0x95, 0x95, 0x95, 0x95, 0x95, 0x95, 0x94, 0x94, 0x94, 0x94, 0x94, 0x94, 0x94, 0x94,
    0x97, 0x97, 0x97, 0x97, 0x97, 0x97, 0x97, 0x97, 0x96, 0x96, 0x96, 0x96, 0x96, 0x96, 0x96, 0x96,
    0x91, 0x91, 0x91, 0x91, 0x91, 0x91, 0x91, 0x91, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x92, 0x92, 0x92, 0x92, 0x92, 0x92, 0x92, 0x92,
    0x9D, 0x9D, 0x9D, 0x9D, 0x9D, 0x9D, 0x9D, 0x9D, 0x9C, 0x9C, 0x9C, 0x9C, 0x9C, 0x9C, 0x9C, 0x9C,
    0x9F, 0x9F, 0x9F, 0x9F, 0x9F, 0x9F, 0x9F, 0x9F, 0x9E, 0x9E, 0x9E, 0x9E, 0x9E, 0x9E, 0x9E, 0x9E,
    0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x98, 0x98, 0x98, 0x98, 0x98, 0x98, 0x98, 0x98,
    0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9A, 0x9A, 0x9A, 0x9A, 0x9A, 0x9A, 0x9A, 0x9A,
];

// A-law decoding lookup table (8-bit to 16-bit)
const ALAW_DEC_TABLE: &[i16; 256] = &[
    -5504, -5248, -6016, -5760, -4480, -4224, -4992, -4736, -7552, -7296, -8064, -7808, -6528,
    -6272, -7040, -6784, -2752, -2624, -3008, -2880, -2240, -2112, -2496, -2368, -3776, -3648,
    -4032, -3904, -3264, -3136, -3520, -3392, -22016, -20992, -24064, -23040, -17920, -16896,
    -19968, -18944, -30208, -29184, -32256, -31232, -26112, -25088, -28160, -27136, -11008, -10496,
    -12032, -11520, -8960, -8448, -9984, -9472, -15104, -14592, -16128, -15616, -13056, -12544,
    -14080, -13568, -344, -328, -376, -360, -280, -264, -312, -296, -472, -456, -504, -488, -408,
    -392, -440, -424, -88, -72, -120, -104, -24, -8, -56, -40, -216, -200, -248, -232, -152, -136,
    -184, -168, -1376, -1312, -1504, -1440, -1120, -1056, -1248, -1184, -1888, -1824, -2016, -1952,
    -1632, -1568, -1760, -1696, -688, -656, -752, -720, -560, -528, -624, -592, -944, -912, -1008,
    -976, -816, -784, -880, -848, 5504, 5248, 6016, 5760, 4480, 4224, 4992, 4736, 7552, 7296, 8064,
    7808, 6528, 6272, 7040, 6784, 2752, 2624, 3008, 2880, 2240, 2112, 2496, 2368, 3776, 3648, 4032,
    3904, 3264, 3136, 3520, 3392, 22016, 20992, 24064, 23040, 17920, 16896, 19968, 18944, 30208,
    29184, 32256, 31232, 26112, 25088, 28160, 27136, 11008, 10496, 12032, 11520, 8960, 8448, 9984,
    9472, 15104, 14592, 16128, 15616, 13056, 12544, 14080, 13568, 344, 328, 376, 360, 280, 264,
    312, 296, 472, 456, 504, 488, 408, 392, 440, 424, 88, 72, 120, 104, 24, 8, 56, 40, 216, 200,
    248, 232, 152, 136, 184, 168, 1376, 1312, 1504, 1440, 1120, 1056, 1248, 1184, 1888, 1824, 2016,
    1952, 1632, 1568, 1760, 1696, 688, 656, 752, 720, 560, 528, 624, 592, 944, 912, 1008, 976, 816,
    784, 880, 848,
];

// Generate full encode table at runtime using decode table
fn generate_ulaw_encode_table() -> [u8; 65536] {
    let mut table = [0u8; 65536];
    for i in 0..65536 {
        let pcm = (i as i32) - 32768; // Convert to signed
                                      // Find closest match in decode table
        let mut best_code = 0u8;
        let mut best_error = i32::MAX;
        for code in 0..256 {
            let decoded = ULAW_DEC_TABLE[code] as i32;
            let error = (decoded - pcm).abs();
            if error < best_error {
                best_error = error;
                best_code = code as u8;
            }
        }
        table[i] = best_code;
    }
    table
}

fn generate_alaw_encode_table() -> [u8; 65536] {
    let mut table = [0u8; 65536];
    for i in 0..65536 {
        let pcm = (i as i32) - 32768; // Convert to signed
                                      // Find closest match in decode table
        let mut best_code = 0u8;
        let mut best_error = i32::MAX;
        for code in 0..256 {
            let decoded = ALAW_DEC_TABLE[code] as i32;
            let error = (decoded - pcm).abs();
            if error < best_error {
                best_error = error;
                best_code = code as u8;
            }
        }
        table[i] = best_code;
    }
    table
}

lazy_static::lazy_static! {
    static ref ULAW_FULL_ENC_TABLE: [u8; 65536] = generate_ulaw_encode_table();
    static ref ALAW_FULL_ENC_TABLE: [u8; 65536] = generate_alaw_encode_table();
}

/// Encode PCM to μ-law
pub fn encode_ulaw(pcm: i16) -> u8 {
    let index = ((pcm as i32) + 32768) as usize;
    ULAW_FULL_ENC_TABLE[index]
}

/// Decode μ-law to PCM
pub fn decode_ulaw(ulaw: u8) -> i16 {
    ULAW_DEC_TABLE[ulaw as usize]
}

/// Encode PCM to A-law
pub fn encode_alaw(pcm: i16) -> u8 {
    let index = ((pcm as i32) + 32768) as usize;
    ALAW_FULL_ENC_TABLE[index]
}

/// Decode A-law to PCM
pub fn decode_alaw(alaw: u8) -> i16 {
    ALAW_DEC_TABLE[alaw as usize]
}

/// G.711 μ-law codec (PCMU)
pub struct G711MuLaw {
    sample_rate: u32,
}

impl G711MuLaw {
    /// Create a new G.711 μ-law codec
    pub fn new(sample_rate: u32) -> Self {
        Self { sample_rate }
    }
}

impl AudioCodec for G711MuLaw {
    fn name(&self) -> &str {
        "G.711 μ-law (PCMU)"
    }

    fn native_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.sample_rate,
            channels: 1,
            codec: crate::AudioCodecType::PCMU,
        }
    }

    fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        Ok(pcm.iter().map(|&sample| encode_ulaw(sample)).collect())
    }

    fn decode(&mut self, encoded: &[u8]) -> Result<Vec<i16>> {
        Ok(encoded.iter().map(|&byte| decode_ulaw(byte)).collect())
    }

    fn frame_size(&self) -> Option<usize> {
        None
    }
}

/// G.711 A-law codec (PCMA)
pub struct G711ALaw {
    sample_rate: u32,
}

impl G711ALaw {
    /// Create a new G.711 A-law codec
    pub fn new(sample_rate: u32) -> Self {
        Self { sample_rate }
    }
}

impl AudioCodec for G711ALaw {
    fn name(&self) -> &str {
        "G.711 A-law (PCMA)"
    }

    fn native_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.sample_rate,
            channels: 1,
            codec: crate::AudioCodecType::PCMA,
        }
    }

    fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        Ok(pcm.iter().map(|&sample| encode_alaw(sample)).collect())
    }

    fn decode(&mut self, encoded: &[u8]) -> Result<Vec<i16>> {
        Ok(encoded.iter().map(|&byte| decode_alaw(byte)).collect())
    }

    fn frame_size(&self) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mulaw_encode_decode_zero() {
        let pcm = vec![0i16];
        let mut codec = G711MuLaw::new(8000);

        let encoded = codec.encode(&pcm).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert!((decoded[0] - pcm[0]).abs() < 50);
    }

    #[test]
    fn test_mulaw_encode_decode_positive() {
        let pcm = vec![1000i16, 5000i16, 10000i16];
        let mut codec = G711MuLaw::new(8000);

        let encoded = codec.encode(&pcm).unwrap();
        assert_eq!(encoded.len(), pcm.len());

        let decoded = codec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), pcm.len());

        for (original, recovered) in pcm.iter().zip(decoded.iter()) {
            let error = (original - recovered).abs();
            assert!(
                error < 500,
                "Error too large: {} vs {}",
                original,
                recovered
            );
        }
    }

    #[test]
    fn test_mulaw_encode_decode_negative() {
        let pcm = vec![-1000i16, -5000i16, -10000i16];
        let mut codec = G711MuLaw::new(8000);

        let encoded = codec.encode(&pcm).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        for (original, recovered) in pcm.iter().zip(decoded.iter()) {
            let error = (original - recovered).abs();
            assert!(error < 500);
        }
    }

    #[test]
    fn test_alaw_encode_decode_zero() {
        let pcm = vec![0i16];
        let mut codec = G711ALaw::new(8000);

        let encoded = codec.encode(&pcm).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert!((decoded[0] - pcm[0]).abs() < 50);
    }

    #[test]
    fn test_alaw_encode_decode_positive() {
        let pcm = vec![1000i16, 5000i16, 10000i16];
        let mut codec = G711ALaw::new(8000);

        let encoded = codec.encode(&pcm).unwrap();
        assert_eq!(encoded.len(), pcm.len());

        let decoded = codec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), pcm.len());

        for (original, recovered) in pcm.iter().zip(decoded.iter()) {
            let error = (original - recovered).abs();
            assert!(error < 500);
        }
    }

    #[test]
    fn test_alaw_encode_decode_negative() {
        let pcm = vec![-1000i16, -5000i16, -10000i16];
        let mut codec = G711ALaw::new(8000);

        let encoded = codec.encode(&pcm).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        for (original, recovered) in pcm.iter().zip(decoded.iter()) {
            let error = (original - recovered).abs();
            assert!(error < 500);
        }
    }

    #[test]
    fn test_mulaw_vs_alaw() {
        let pcm = vec![5000i16];
        let mut mulaw = G711MuLaw::new(8000);
        let mut alaw = G711ALaw::new(8000);

        let mulaw_encoded = mulaw.encode(&pcm).unwrap();
        let alaw_encoded = alaw.encode(&pcm).unwrap();

        assert_ne!(mulaw_encoded, alaw_encoded);
    }
}
