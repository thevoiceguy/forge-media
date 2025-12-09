# DTMF Integration Guide

Comprehensive guide for integrating DTMF (Dual-Tone Multi-Frequency) support across forge-media and siphon-rs.

## Overview

Forge Media Engine supports three DTMF transport methods:

1. **RFC 2833 (telephone-event)** - RTP payload type for DTMF ✅ IMPLEMENTED
2. **Inband DTMF** - Audio frequency detection ✅ IMPLEMENTED
3. **SIP INFO** - SIP signaling method ⏳ REQUIRES siphon-rs

## 1. RFC 2833 (telephone-event)

**Status**: ✅ Complete in `forge-dtmf`

### RTP Payload Format

RFC 2833 uses a dedicated RTP payload type (typically 101) to carry DTMF events:

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|     event     |E|R| volume    |          duration             |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

### Usage in forge-media

**Detection**:
```rust
use forge_dtmf::{Rfc2833Detector, DtmfDetector};

let mut detector = Rfc2833Detector::new(8000); // 8kHz sample rate

// When RTP packet with payload type 101 arrives:
let events = detector.process_with_timestamp(&rtp_payload, rtp_timestamp)?;
for event in events {
    println!("DTMF: {} ({:?})", event.digit, event.event_type);
}
```

**Generation**:
```rust
use forge_dtmf::{Rfc2833Generator, DtmfDigit};

let mut generator = Rfc2833Generator::new(8000, 20); // 8kHz, 20ms packets

// Start digit '5'
let start_packet = generator.start_digit(DtmfDigit::Five);

// Send continuation packets every 20ms
while digit_pressed {
    if let Some(cont_packet) = generator.continue_digit() {
        send_rtp(101, cont_packet.to_bytes());
    }
}

// End digit (sends 3 copies for reliability)
if let Some(end_packets) = generator.end_digit() {
    for packet in end_packets {
        send_rtp(101, packet.to_bytes());
    }
}
```

### Integration Points

1. **RTP Parser**: Detect payload type 101 in `forge-rtp`
2. **Forwarding Engine**: Route RFC 2833 packets to DTMF detector
3. **Event Bus**: Publish detected DTMF events for application consumption

## 2. Inband DTMF Detection

**Status**: ✅ Complete in `forge-dtmf`

### Goertzel Algorithm

Detects DTMF tones by analyzing audio frequency content using Goertzel filters for the 8 DTMF frequencies:

| Low Freq | High Freq → | 1209 Hz | 1336 Hz | 1477 Hz | 1633 Hz |
|----------|-------------|---------|---------|---------|---------|
| **697 Hz** | | 1 | 2 | 3 | A |
| **770 Hz** | | 4 | 5 | 6 | B |
| **852 Hz** | | 7 | 8 | 9 | C |
| **941 Hz** | | * | 0 | # | D |

### Usage

```rust
use forge_dtmf::{GoertzelDetector, DtmfDetector};

let mut detector = GoertzelDetector::new(
    8000,  // Sample rate
    160    // Frame size (20ms at 8kHz)
);

// Process PCM audio samples
let pcm_samples: Vec<i16> = decode_audio_frame();
let events = detector.process_samples(&pcm_samples)?;

for event in events {
    match event.event_type {
        DtmfEventType::Start => println!("Digit {} pressed", event.digit),
        DtmfEventType::End => println!("Digit {} released ({}ms)",
            event.digit, event.duration_ms.unwrap()),
        _ => {}
    }
}
```

### Configuration

```rust
// Adjust sensitivity
detector.set_energy_threshold(500000.0);  // Lower = more sensitive

// Adjust twist tolerance (low/high frequency power ratio)
detector.set_twist_threshold(2.0);  // 6dB tolerance
```

### Integration Points

1. **Media Processor**: Tap audio stream before/after codec
2. **Codec Pipeline**: Run detection on decoded PCM
3. **Event Bus**: Publish detected events

## 3. SIP INFO Method

**Status**: ⏳ REQUIRES siphon-rs implementation

### Protocol

SIP INFO messages carry DTMF as application/dtmf-relay:

```
INFO sip:user@example.com SIP/2.0
Via: SIP/2.0/UDP 192.168.1.100:5060
From: <sip:alice@example.com>;tag=1234
To: <sip:bob@example.com>;tag=5678
Call-ID: abc123@example.com
CSeq: 1 INFO
Content-Type: application/dtmf-relay
Content-Length: 24

Signal=5
Duration=160
```

### Required in siphon-rs

**1. SIP INFO Request Handling**

```rust
// In siphon-rs SIP stack
pub struct SipInfoRequest {
    pub content_type: String,
    pub body: Vec<u8>,
}

impl SipStack {
    pub fn on_info_request(&mut self, req: SipInfoRequest) -> Result<()> {
        if req.content_type == "application/dtmf-relay" {
            self.handle_dtmf_info(req.body)?;
        }
        // Send 200 OK
        self.send_response(200, "OK")
    }

    fn handle_dtmf_info(&self, body: Vec<u8>) -> Result<()> {
        let dtmf = parse_dtmf_relay(&body)?;

        // Forward to forge-media event bus
        self.event_bus.publish(DtmfEvent {
            digit: dtmf.signal,
            event_type: if dtmf.duration > 0 {
                DtmfEventType::End
            } else {
                DtmfEventType::Start
            },
            method: DtmfMethod::SipInfo,
            duration_ms: Some(dtmf.duration),
            timestamp: None,
        })?;

        Ok(())
    }
}
```

**2. SIP INFO Request Generation**

```rust
impl SipStack {
    pub fn send_dtmf_info(&mut self, dialog: &Dialog, digit: char, duration: u32) -> Result<()> {
        let body = format!("Signal={}\r\nDuration={}\r\n", digit, duration);

        let info = SipRequest::new(Method::INFO)
            .with_header("Content-Type", "application/dtmf-relay")
            .with_body(body.into_bytes());

        self.send_request(dialog, info)
    }
}
```

**3. Parser for application/dtmf-relay**

```rust
struct DtmfRelay {
    signal: char,
    duration: u32,
}

fn parse_dtmf_relay(body: &[u8]) -> Result<DtmfRelay> {
    let text = String::from_utf8(body.to_vec())?;

    let mut signal = None;
    let mut duration = 0;

    for line in text.lines() {
        let parts: Vec<&str> = line.split('=').collect();
        if parts.len() == 2 {
            match parts[0] {
                "Signal" => signal = Some(parts[1].chars().next().unwrap()),
                "Duration" => duration = parts[1].parse()?,
                _ => {}
            }
        }
    }

    Ok(DtmfRelay {
        signal: signal.ok_or("Missing Signal field")?,
        duration,
    })
}
```

### Integration with forge-media

**forge-media Event Bus Bridge**:

```rust
// In forge-engine or forge-api
pub struct DtmfBridge {
    event_bus: Arc<EventBus>,
}

impl DtmfBridge {
    pub fn on_sip_info_dtmf(&self, call_id: &str, digit: char, duration: u32) -> Result<()> {
        let dtmf_digit = DtmfDigit::from_char(digit)?;

        let event = DtmfEvent::with_duration(
            dtmf_digit,
            if duration > 0 { DtmfEventType::End } else { DtmfEventType::Start },
            DtmfMethod::SipInfo,
            duration,
        );

        self.event_bus.publish(Event::Dtmf {
            call_id: call_id.to_string(),
            event,
        })?;

        Ok(())
    }
}
```

## Unified DTMF Event System

### Event Bus Architecture

```rust
// In forge-core
pub enum Event {
    Dtmf {
        call_id: String,
        session_id: Option<String>,
        event: DtmfEvent,
    },
    // ... other events
}

// Application subscribes to DTMF events
event_bus.subscribe(|event| {
    if let Event::Dtmf { call_id, event } = event {
        match event.method {
            DtmfMethod::Rfc2833 => handle_rfc2833(call_id, event),
            DtmfMethod::Inband => handle_inband(call_id, event),
            DtmfMethod::SipInfo => handle_sip_info(call_id, event),
        }
    }
});
```

### Example: IVR Application

```rust
struct IvrSession {
    digits_collected: String,
    dtmf_timeout: Duration,
}

impl IvrSession {
    fn on_dtmf_event(&mut self, event: DtmfEvent) {
        match event.event_type {
            DtmfEventType::Start => {
                self.digits_collected.push(event.digit.to_string().chars().next().unwrap());
                self.reset_timeout();

                // Provide feedback
                if event.digit == DtmfDigit::Hash {
                    self.process_input();
                }
            }
            DtmfEventType::End => {
                // Log duration for analytics
                tracing::info!("DTMF {} held for {}ms via {:?}",
                    event.digit, event.duration_ms.unwrap(), event.method);
            }
            _ => {}
        }
    }
}
```

## Priority and Conflict Resolution

When multiple DTMF methods are active:

1. **RFC 2833** - Highest priority (most reliable, lowest latency)
2. **SIP INFO** - Medium priority (depends on SIP transport delay)
3. **Inband** - Lowest priority (fallback, higher latency)

**Deduplication Strategy**:

```rust
struct DtmfDeduplicator {
    last_event: Option<(DtmfDigit, Instant, DtmfMethod)>,
    dedup_window: Duration,
}

impl DtmfDeduplicator {
    fn should_process(&mut self, event: &DtmfEvent) -> bool {
        if let Some((last_digit, last_time, last_method)) = self.last_event {
            let elapsed = last_time.elapsed();

            if event.digit == last_digit && elapsed < self.dedup_window {
                // Same digit within dedup window - check priority
                return event.method.priority() > last_method.priority();
            }
        }

        self.last_event = Some((event.digit, Instant::now(), event.method));
        true
    }
}
```

## Testing

### Unit Tests

All DTMF methods have comprehensive unit tests in `forge-dtmf`:
- RFC 2833 parsing/generation: ✅ 15 tests passing
- Inband detection: ✅ Goertzel algorithm validated
- All 16 DTMF digits: ✅ Tested (0-9, *, #, A-D)

### Integration Testing

**Test RFC 2833 with RTP**:
```bash
# Generate DTMF digits via RFC 2833
cargo run --example dtmf_rfc2833_test -- --digit 5 --duration 200
```

**Test Inband Detection**:
```bash
# Generate audio tones and detect
cargo run --example dtmf_inband_test -- --digit 5 --sample-rate 8000
```

### End-to-End Testing

Requires both forge-media and siphon-rs:

```bash
# Terminal 1: Start forge-media
RUST_LOG=forge=debug ./forge-media

# Terminal 2: Start siphon-rs SIP server with DTMF support
./siphon-rs-server --dtmf-methods rfc2833,info,inband

# Terminal 3: Make call and send DTMF
./test-dtmf-call.sh
```

## References

- [RFC 2833](https://www.rfc-editor.org/rfc/rfc2833.html) - RTP Payload for DTMF Digits, Telephony Tones and Signals
- [RFC 4733](https://www.rfc-editor.org/rfc/rfc4733.html) - RTP Payload for DTMF Digits (obsoletes 2833)
- [RFC 6086](https://www.rfc-editor.org/rfc/rfc6086.html) - Session Initiation Protocol (SIP) INFO Method and Package Framework
- [ITU-T Q.23](https://www.itu.int/rec/T-REC-Q.23) - Technical features of push-button telephone sets

## Next Steps

1. ✅ RFC 2833 detection and generation - COMPLETE
2. ✅ Inband detection with Goertzel - COMPLETE
3. ⏳ Integrate with RTP forwarding engine
4. ⏳ Add DTMF event bus in forge-core
5. ⏳ Implement SIP INFO in siphon-rs
6. ⏳ Create end-to-end test suite
