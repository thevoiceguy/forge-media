//! G.729 RTP Integration Example
//!
//! Demonstrates how to use G.729 codec with RTP including:
//! - Frame-by-frame encoding for RTP packets
//! - Packet loss concealment (PLC)
//! - Variable-length frame handling with VAD
//! - Sequence number tracking
//!
//! Run with:
//!   cargo run --example g729_rtp --features g729

use std::collections::VecDeque;

#[cfg(feature = "g729")]
use forge_codecs::g729::{G729Codec, G729Variant};

#[cfg(feature = "g729")]
use forge_codecs::CodecError;

/// Simulated RTP packet
#[derive(Debug, Clone)]
struct RtpPacket {
    sequence: u16,
    timestamp: u32,
    payload: Vec<u8>,
}

/// RTP session with G.729 codec and packet loss handling
#[cfg(feature = "g729")]
struct G729RtpSession {
    codec: G729Codec,
    last_sequence: u16,
    timestamp: u32,
    packets_sent: usize,
    packets_lost: usize,
}

#[cfg(feature = "g729")]
impl G729RtpSession {
    /// Create new RTP session with G.729
    fn new(variant: G729Variant) -> Result<Self, CodecError> {
        Ok(Self {
            codec: G729Codec::new_with_variant(variant)?,
            last_sequence: 0,
            timestamp: 0,
            packets_sent: 0,
            packets_lost: 0,
        })
    }

    /// Encode PCM audio into an RTP packet
    ///
    /// Input: 80 samples @ 8kHz (10ms frame)
    /// Output: RTP packet with G.729 payload (0, 2, or 10 bytes depending on VAD)
    fn encode_rtp(&mut self, pcm: &[i16; 80]) -> Result<RtpPacket, CodecError> {
        // Encode frame without length prefix (RTP provides framing)
        let payload = self.codec.encode_frame_unframed(pcm)?;

        // Create RTP packet
        let packet = RtpPacket {
            sequence: self.last_sequence,
            timestamp: self.timestamp,
            payload,
        };

        // Update state
        self.last_sequence = self.last_sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(80); // 80 samples per frame
        self.packets_sent += 1;

        Ok(packet)
    }

    /// Decode RTP packet to PCM audio
    ///
    /// Handles packet loss with PLC when packets are missing
    fn decode_rtp(&mut self, packet: Option<&RtpPacket>) -> Result<Vec<i16>, CodecError> {
        match packet {
            Some(pkt) => {
                // Check for sequence gap (packet loss)
                let expected = self.last_sequence.wrapping_add(1);
                let is_gap = pkt.sequence != expected && self.last_sequence != 0;

                if is_gap {
                    // Calculate how many packets were lost
                    let gap_size = pkt.sequence.wrapping_sub(expected) as usize;
                    self.packets_lost += gap_size;

                    println!(
                        "  ⚠ Detected packet loss: {} packet(s) (seq {} -> {})",
                        gap_size, self.last_sequence, pkt.sequence
                    );

                    // Conceal lost packets with PLC
                    let mut output = Vec::new();
                    for i in 0..gap_size {
                        println!("    → Concealing lost packet {} with PLC", i + 1);
                        let concealed = self.codec.decode_frame_with_plc(&[], true)?;
                        output.extend_from_slice(&concealed);
                    }

                    // Then decode current packet normally
                    let current = self.codec.decode_frame_with_plc(&pkt.payload, false)?;
                    output.extend_from_slice(&current);

                    self.last_sequence = pkt.sequence;
                    Ok(output)
                } else {
                    // Normal path: no packet loss
                    self.last_sequence = pkt.sequence;
                    self.codec.decode_frame_with_plc(&pkt.payload, false)
                }
            }
            None => {
                // Explicit packet loss (e.g., jitter buffer timeout)
                self.packets_lost += 1;
                println!("  ⚠ Packet loss detected, using PLC");
                self.codec.decode_frame_with_plc(&[], true)
            }
        }
    }

    /// Get packet loss statistics
    fn stats(&self) -> (usize, usize, f64) {
        let loss_rate = if self.packets_sent > 0 {
            (self.packets_lost as f64 / self.packets_sent as f64) * 100.0
        } else {
            0.0
        };
        (self.packets_sent, self.packets_lost, loss_rate)
    }
}

/// Simulate a network with packet loss
#[cfg(feature = "g729")]
struct NetworkSimulator {
    packets: VecDeque<RtpPacket>,
    loss_rate: f64, // 0.0 to 1.0
}

#[cfg(feature = "g729")]
impl NetworkSimulator {
    fn new(loss_rate: f64) -> Self {
        Self {
            packets: VecDeque::new(),
            loss_rate: loss_rate.clamp(0.0, 1.0),
        }
    }

    fn send(&mut self, packet: RtpPacket) {
        // Simulate packet loss
        if rand::random::<f64>() >= self.loss_rate {
            self.packets.push_back(packet);
        }
    }

    fn receive(&mut self) -> Option<RtpPacket> {
        self.packets.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
}

// Use a simple LCG for reproducible random numbers
mod rand {
    use std::cell::Cell;

    thread_local! {
        static SEED: Cell<u32> = Cell::new(12345);
    }

    pub fn random<T: From<f64>>() -> T {
        SEED.with(|seed| {
            let s = seed.get();
            let next = s.wrapping_mul(1103515245).wrapping_add(12345);
            seed.set(next);
            T::from((next >> 16) as f64 / 32768.0)
        })
    }
}

#[cfg(feature = "g729")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("G.729 RTP Integration Example");
    println!("=============================\n");

    // Example 1: Basic RTP encoding/decoding without packet loss
    println!("Example 1: Basic RTP Transmission (No Packet Loss)");
    println!("-------------------------------------------------");
    basic_rtp_example()?;

    println!("\n");

    // Example 2: RTP with packet loss and PLC
    println!("Example 2: RTP with Packet Loss and PLC");
    println!("----------------------------------------");
    packet_loss_example()?;

    println!("\n");

    // Example 3: VAD demonstration
    println!("Example 3: Voice Activity Detection (VAD)");
    println!("------------------------------------------");
    vad_example()?;

    Ok(())
}

#[cfg(feature = "g729")]
fn basic_rtp_example() -> Result<(), Box<dyn std::error::Error>> {
    let mut sender = G729RtpSession::new(G729Variant::G729A)?;
    let mut receiver = G729RtpSession::new(G729Variant::G729A)?;

    println!("Encoding and transmitting 5 RTP packets...\n");

    for i in 0..5 {
        // Generate test audio (sine wave)
        let mut pcm = [0i16; 80];
        for (j, sample) in pcm.iter_mut().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * 440.0 * (i * 80 + j) as f64 / 8000.0;
            *sample = (phase.sin() * 8000.0) as i16;
        }

        // Encode to RTP packet
        let packet = sender.encode_rtp(&pcm)?;
        println!(
            "Packet {}: seq={:5}, ts={:8}, payload={:2} bytes",
            i,
            packet.sequence,
            packet.timestamp,
            packet.payload.len()
        );

        // Decode (no packet loss)
        let decoded = receiver.decode_rtp(Some(&packet))?;
        assert_eq!(decoded.len(), 80, "Decoded frame should be 80 samples");
    }

    let (sent, lost, loss_rate) = sender.stats();
    println!("\nStatistics: {} sent, {} lost ({:.1}% loss rate)", sent, lost, loss_rate);

    Ok(())
}

#[cfg(feature = "g729")]
fn packet_loss_example() -> Result<(), Box<dyn std::error::Error>> {
    let mut sender = G729RtpSession::new(G729Variant::G729A)?;
    let mut receiver = G729RtpSession::new(G729Variant::G729A)?;
    let mut network = NetworkSimulator::new(0.2); // 20% packet loss

    println!("Encoding 10 packets with 20% simulated packet loss...\n");

    // Send packets through lossy network
    for i in 0..10 {
        let mut pcm = [0i16; 80];
        for (j, sample) in pcm.iter_mut().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * 440.0 * (i * 80 + j) as f64 / 8000.0;
            *sample = (phase.sin() * 8000.0) as i16;
        }

        let packet = sender.encode_rtp(&pcm)?;
        network.send(packet);
    }

    println!("Receiving and decoding with PLC...\n");

    // Receive and decode with gap detection
    for i in 0..10 {
        let packet = network.receive();
        if let Some(ref pkt) = packet {
            println!(
                "Packet {}: seq={:5}, payload={:2} bytes",
                i,
                pkt.sequence,
                pkt.payload.len()
            );
        } else {
            println!("Packet {}: LOST", i);
        }

        let decoded = receiver.decode_rtp(packet.as_ref())?;
        println!("  → Decoded {} samples", decoded.len());
    }

    // Drain any remaining packets
    while !network.is_empty() {
        if let Some(packet) = network.receive() {
            let _ = receiver.decode_rtp(Some(&packet))?;
        }
    }

    let (sent, lost, loss_rate) = receiver.stats();
    println!("\nStatistics: {} received, {} lost ({:.1}% loss rate)", sent, lost, loss_rate);

    Ok(())
}

#[cfg(feature = "g729")]
fn vad_example() -> Result<(), Box<dyn std::error::Error>> {
    let mut sender = G729RtpSession::new(G729Variant::G729B)?; // VAD enabled
    let mut receiver = G729RtpSession::new(G729Variant::G729B)?;

    println!("Encoding with VAD (G.729 Annex B)...\n");
    println!("Speech frames → 10 bytes");
    println!("SID frames    → 2 bytes (silence descriptor)");
    println!("No TX frames  → 0 bytes (silence)\n");

    // Simulate conversation: speech, silence, speech
    let frames = [
        ("Speech", vec![1000i16; 80]),   // Active speech
        ("Speech", vec![800i16; 80]),    // Active speech
        ("Silence", vec![10i16; 80]),    // Background noise
        ("Silence", vec![5i16; 80]),     // Background noise
        ("Silence", vec![8i16; 80]),     // Background noise
        ("Speech", vec![1200i16; 80]),   // Active speech
        ("Speech", vec![900i16; 80]),    // Active speech
    ];

    for (i, (label, pcm)) in frames.iter().enumerate() {
        let pcm_array: [i16; 80] = pcm.as_slice().try_into().unwrap();
        let packet = sender.encode_rtp(&pcm_array)?;

        let frame_type = match packet.payload.len() {
            10 => "SPEECH (10 bytes)",
            2 => "SID    ( 2 bytes)",
            0 => "NO_TX  ( 0 bytes)",
            n => &format!("UNKNOWN ({} bytes)", n),
        };

        println!(
            "Frame {}: {:7} → {} | seq={}, ts={}",
            i, label, frame_type, packet.sequence, packet.timestamp
        );

        // Decode
        let decoded = receiver.decode_rtp(Some(&packet))?;
        assert_eq!(decoded.len(), 80);
    }

    println!("\nVAD automatically detects speech vs. silence and adjusts payload size.");
    println!("Bandwidth savings: ~40-50% during typical conversation.");

    Ok(())
}

#[cfg(not(feature = "g729"))]
fn main() {
    eprintln!("This example requires the 'g729' feature.");
    eprintln!("Run with: cargo run --example g729_rtp --features g729");
    std::process::exit(1);
}
