# G.729 Codec Integration Guide

## Overview

This guide covers the integration and usage of the G.729 audio codec in Forge Media Engine. G.729 is a narrowband audio codec operating at 8 kbps, designed for low-bandwidth scenarios like mobile networks and satellite links.

## Features

- **G.729 Annex A**: Standard 8 kbit/s codec with high compression
- **G.729 Annex B**: Voice Activity Detection (VAD) and Discontinuous Transmission (DTX)
- **Packet Loss Concealment (PLC)**: Error concealment for missing RTP packets
- **Length-Prefixed Framing**: Safe handling of variable-length frames with VAD
- **bcg729 Integration**: FFI bindings to the battle-tested GPL-licensed bcg729 library

## Installation

### Prerequisites

Forge's G.729 implementation requires the `libbcg729` C library (version 1.0.4 or later).

#### Ubuntu/Debian

```bash
sudo apt-get update
sudo apt-get install libbcg729-dev pkg-config
```

#### Fedora/RHEL

```bash
sudo dnf install bcg729-devel pkg-config
```

#### macOS

```bash
brew install bcg729 pkg-config
```

#### Building from Source

If your distribution doesn't provide bcg729:

```bash
git clone https://gitlab.linphone.org/BC/public/bcg729.git
cd bcg729
mkdir build && cd build
cmake .. -DCMAKE_INSTALL_PREFIX=/usr/local
make
sudo make install
sudo ldconfig  # Linux only
```

### Verification

Verify the library is installed:

```bash
pkg-config --modversion bcg729
# Should output: 1.1.1 (or your installed version)

pkg-config --libs bcg729
# Should output: -lbcg729
```

## Quick Start

### Adding to Your Project

Add to your `Cargo.toml`:

```toml
[dependencies]
forge-codecs = { version = "0.2", features = ["g729"] }

# Or enable all codecs:
forge-codecs = { version = "0.2", features = ["all-codecs"] }
```

### Basic Encoding and Decoding

```rust
use forge_codecs::g729::{G729Codec, G729Variant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create codec (G.729 Annex A)
    let mut codec = G729Codec::new()?;

    // Prepare audio: 80 samples @ 8kHz (10ms frame)
    let pcm_samples: Vec<i16> = vec![100; 80];

    // Encode
    let encoded = codec.encode(&pcm_samples)?;
    println!("Encoded {} samples to {} bytes", pcm_samples.len(), encoded.len());
    // Output: Encoded 80 samples to 11 bytes (1 length + 10 data)

    // Decode
    let decoded = codec.decode(&encoded)?;
    println!("Decoded {} bytes to {} samples", encoded.len(), decoded.len());
    // Output: Decoded 11 bytes to 80 samples

    Ok(())
}
```

### Using G.729 with VAD (Annex B)

```rust
use forge_codecs::g729::{G729Codec, G729Variant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create codec with Voice Activity Detection
    let mut codec = G729Codec::new_with_variant(G729Variant::G729B)?;

    let speech: Vec<i16> = vec![1000; 80];  // Active speech
    let silence: Vec<i16> = vec![10; 80];   // Background noise

    // Encode speech frame
    let encoded_speech = codec.encode(&speech)?;
    println!("Speech frame: {} bytes", encoded_speech.len());
    // Output: Speech frame: 11 bytes (1 + 10)

    // Encode silence frame (VAD may produce SID frame)
    let encoded_silence = codec.encode(&silence)?;
    println!("Silence frame: {} bytes", encoded_silence.len());
    // Output: Silence frame: 3 bytes (1 + 2) for SID frame
    // Or: Silence frame: 1 byte (1 + 0) for no transmission

    Ok(())
}
```

## Framing Format

G.729 with VAD (Annex B) can produce variable-length frames:
- **10 bytes**: Full speech frame
- **2 bytes**: SID (Silence Insertion Descriptor) frame
- **0 bytes**: No transmission frame (DTX)

To safely handle these variable lengths, Forge uses **length-prefixed framing**:

```
Format: [len:u8][data:len bytes][len:u8][data:len bytes]...
```

### Example Frame Sequences

**Speech frames only:**
```
[0x0A][10 bytes of data][0x0A][10 bytes of data][0x0A][10 bytes of data]
```

**Mixed speech and silence:**
```
[0x0A][10 bytes speech][0x02][2 bytes SID][0x00][0x0A][10 bytes speech]
```

### Why Length Prefixing?

Without length prefixes, concatenated frames would be ambiguous:

```
BAD:  [10 bytes][2 bytes][10 bytes]
      Is this: 3 frames? Or 1 x 22-byte frame?

GOOD: [0x0A][10 bytes][0x02][2 bytes][0x0A][10 bytes]
      Unambiguous: exactly 3 frames
```

### Overhead

The length prefix adds **1 byte per frame**:
- 10-byte frame → 11 bytes total (10% overhead)
- 2-byte SID → 3 bytes total (50% overhead)
- 0-byte DTX → 1 byte total (infinite overhead, but saves network bandwidth)

**Impact**: Minimal. The framing overhead is negligible compared to the bandwidth savings from 8 kbps compression and VAD.

## RTP Integration

For real-time RTP scenarios, use the frame-by-frame APIs for precise control over packet boundaries and packet loss concealment.

### Frame-by-Frame Encoding

```rust
use forge_codecs::g729::G729Codec;

fn encode_rtp_packet(codec: &mut G729Codec, pcm: &[i16; 80])
    -> Result<Vec<u8>, Box<dyn std::error::Error>>
{
    // Encode single frame without length prefix (RTP has its own framing)
    let encoded = codec.encode_frame_unframed(pcm)?;

    // encoded.len() is 0, 2, or 10 depending on VAD
    // Send this as RTP payload (RFC 3551 payload type 18)

    Ok(encoded)
}
```

### Packet Loss Concealment

```rust
use forge_codecs::g729::G729Codec;

fn decode_rtp_packet(
    codec: &mut G729Codec,
    rtp_payload: Option<&[u8]>,
    sequence_gap: bool,
) -> Result<Vec<i16>, Box<dyn std::error::Error>>
{
    match rtp_payload {
        Some(data) if !sequence_gap => {
            // Normal packet: decode with PLC disabled
            let pcm = codec.decode_frame_with_plc(data, false)?;
            Ok(pcm)
        }
        _ => {
            // Packet lost: use PLC to conceal erasure
            let pcm = codec.decode_frame_with_plc(&[], true)?;
            Ok(pcm)
        }
    }
}
```

### Complete RTP Example

```rust
use forge_codecs::g729::G729Codec;

struct RtpSession {
    codec: G729Codec,
    last_sequence: u16,
}

impl RtpSession {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            codec: G729Codec::new()?,
            last_sequence: 0,
        })
    }

    fn handle_rtp_packet(
        &mut self,
        sequence: u16,
        payload: &[u8],
    ) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
        // Check for sequence gap (packet loss)
        let expected = self.last_sequence.wrapping_add(1);
        let is_gap = sequence != expected && self.last_sequence != 0;

        if is_gap {
            // Detect how many packets were lost
            let gap_size = sequence.wrapping_sub(expected) as usize;

            // Conceal lost packets with PLC
            let mut pcm_output = Vec::new();
            for _ in 0..gap_size {
                let concealed = self.codec.decode_frame_with_plc(&[], true)?;
                pcm_output.extend_from_slice(&concealed);
            }

            // Then decode current packet normally
            let current = self.codec.decode_frame_with_plc(payload, false)?;
            pcm_output.extend_from_slice(&current);

            self.last_sequence = sequence;
            Ok(pcm_output)
        } else {
            // Normal path: no packet loss
            self.last_sequence = sequence;
            self.codec.decode_frame_with_plc(payload, false)
        }
    }
}
```

## AudioCodec Trait

G.729 implements the `AudioCodec` trait for use in Forge's transcoding pipeline:

```rust
use forge_codecs::{AudioCodec, g729::G729Codec};

fn transcoding_example() -> Result<(), Box<dyn std::error::Error>> {
    let mut codec = G729Codec::new()?;

    // Get codec metadata
    println!("Codec: {}", codec.name());           // "G.729A"
    println!("Frame size: {:?}", codec.frame_size()); // Some(80)

    let format = codec.native_format();
    println!("Sample rate: {} Hz", format.sample_rate);  // 8000
    println!("Channels: {}", format.channels);           // 1

    // Use in transcoding
    let pcm_input: Vec<i16> = vec![0; 160];  // 20ms @ 8kHz
    let encoded = codec.encode(&pcm_input)?;
    let decoded = codec.decode(&encoded)?;

    Ok(())
}
```

## Best Practices

### When to Use G.729

**Good Use Cases:**
- Mobile networks with limited bandwidth
- Satellite links with high latency/cost
- VoIP with many concurrent calls (bandwidth multiplier)
- Legacy system interoperability (widespread support)

**Poor Use Cases:**
- Music or high-fidelity audio (use Opus)
- Wideband/HD Voice (use G.722 or Opus)
- WebRTC (prefer Opus for better quality and browser support)
- Frequent transcoding (quality degrades)

### G.729A vs G.729B

**G.729A** (Standard):
- Always transmits 10-byte frames
- Predictable bandwidth: 8 kbps constant
- Use when bandwidth is plentiful but you need low latency
- Best for continuous speech (customer service, etc.)

**G.729B** (VAD/DTX):
- Variable frame sizes (0, 2, or 10 bytes)
- Bandwidth savings during silence (40-50% typical)
- Use when bandwidth is constrained
- Best for conversational speech with pauses

### Performance Considerations

**Encoding/Decoding Speed:**
- G.729 is computationally expensive (~15-20 MIPS)
- Modern CPUs can handle 100+ concurrent sessions
- Consider CPU budget for large-scale deployments

**Latency:**
- Algorithmic delay: 15ms (look-ahead)
- Frame size: 10ms
- Total one-way delay: ~25ms + network
- Acceptable for most VoIP applications

**Memory:**
- ~32KB per encoder/decoder pair
- Negligible for modern systems

### Common Pitfalls

**1. Forgetting Feature Flag**

```toml
# WRONG - G.729 will not compile
forge-codecs = "0.2"

# CORRECT
forge-codecs = { version = "0.2", features = ["g729"] }
```

**2. Not Handling Initialization Errors**

```rust
// WRONG - can panic
let codec = G729Codec::new().unwrap();

// CORRECT - handle missing library
let codec = match G729Codec::new() {
    Ok(c) => c,
    Err(e) => {
        eprintln!("G.729 not available: {}", e);
        return Err(e.into());
    }
};
```

**3. Mixing Framed and Unframed APIs**

```rust
// WRONG - mixing formats
let framed = codec.encode(&pcm)?;           // Has length prefix
let decoded = codec.decode_frame_with_plc(&framed, false)?; // Expects raw frame

// CORRECT - use matching pair
let unframed = codec.encode_frame_unframed(&pcm)?;
let decoded = codec.decode_frame_with_plc(&unframed, false)?;
```

**4. Ignoring Packet Loss**

```rust
// WRONG - treats packet loss as silence
if rtp_payload.is_none() {
    return Ok(vec![0; 80]);  // Generates clicks/pops
}

// CORRECT - use PLC
let pcm = codec.decode_frame_with_plc(&[], true)?;  // Smooth concealment
```

## Testing

### Unit Tests

```bash
# Run G.729 tests (requires libbcg729)
cargo test --features g729 g729

# Run all codec tests
cargo test --features all-codecs
```

### Integration Testing

```bash
# Test with real audio file (requires sox)
sox input.wav -r 8000 -c 1 -t raw -e signed-integer -b 16 input.pcm

# Encode with G.729
cargo run --example g729_encode --features g729 -- input.pcm output.g729

# Decode
cargo run --example g729_decode --features g729 -- output.g729 output.pcm

# Listen to result
sox -r 8000 -c 1 -t raw -e signed-integer -b 16 output.pcm output.wav
play output.wav
```

## Troubleshooting

### Compilation Errors

**Error**: `Could not find libbcg729`

```bash
# Check if installed
pkg-config --modversion bcg729

# If not found, install (see Installation section above)
sudo apt-get install libbcg729-dev  # Ubuntu/Debian
```

**Error**: `linking with 'cc' failed`

```bash
# Update pkg-config path
export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH

# Or specify library path
export LD_LIBRARY_PATH=/usr/local/lib:$LD_LIBRARY_PATH
```

### Runtime Errors

**Error**: `Codec initialization failed: encoder creation failed`

- Verify bcg729 library is in library path
- Check version compatibility (need 1.0.4+)
- Try rebuilding with `cargo clean && cargo build`

**Error**: `Invalid G.729 frame length: 15 bytes`

- You're trying to decode with the wrong API
- Use `decode_frame_with_plc()` for RTP (unframed)
- Use `decode()` for framed format (AudioCodec trait)

**Error**: `Truncated G.729 frame`

- Corrupted data or incorrect framing
- Verify length-prefix format is correct
- Check for buffer truncation during transmission

## License Note

**bcg729** is licensed under **GNU General Public License v3.0 (GPL-3.0)**.

**Important**: If you enable the `g729` feature and link against bcg729, your binary becomes subject to the GPL-3.0 license terms. This means:

- You must provide source code to users
- Derivative works must also be GPL-3.0
- Commercial use requires GPL compliance

**Patent Status**: G.729 patents expired in 2017. The codec is now royalty-free to use.

For commercial deployments where GPL is not acceptable, consider:
- Using Opus (royalty-free, permissive license)
- Licensing a proprietary G.729 implementation
- Keeping G.729 in a separate GPL-licensed binary/process

## Further Reading

- [RFC 3551](https://tools.ietf.org/html/rfc3551) - RTP Profile for Audio/Video (G.729 payload type)
- [RFC 3555](https://tools.ietf.org/html/rfc3555) - MIME Type Registration of RTP Payload Formats
- [ITU-T G.729](https://www.itu.int/rec/T-REC-G.729/) - Official specification
- [bcg729 Library](https://gitlab.linphone.org/BC/public/bcg729) - Source repository
- [Forge Architecture](../FORGE%20ARCHITECTURE.md) - System design
- [Codec Comparison](../README.md#codec-support) - When to use which codec

## Support

For issues or questions:
- [GitHub Issues](https://github.com/ferrous-comms/forge-media/issues)
- Check [KNOWN_ISSUES.md](../KNOWN_ISSUES.md) for current limitations
- Review [CHANGELOG.md](../CHANGELOG.md) for recent changes
