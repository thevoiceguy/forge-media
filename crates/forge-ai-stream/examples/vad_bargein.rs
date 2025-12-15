//! VAD and Barge-in Detection Example
//!
//! Demonstrates Voice Activity Detection and automatic barge-in/interrupt
//! handling when the user speaks over the AI.
//!
//! Usage:
//!   cargo run --example vad_bargein

use forge_ai_stream::{
    BargeInConfig, BargeInDetector, VadConfig, VadDetector, VadState,
};
use std::time::Duration;

fn main() {
    println!("=== Voice Activity Detection Demo ===\n");

    // Configure VAD
    let vad_config = VadConfig {
        sensitivity: 0.6,
        min_speech_duration_ms: 100,
        min_silence_duration_ms: 500,
        sample_rate: 16000,
        frame_size_ms: 20,
        energy_threshold: 0.0, // Auto-adaptive
        zcr_threshold: 0.3,
    };

    let mut vad = VadDetector::new(vad_config);
    println!("✓ VAD initialized with adaptive threshold");

    // Simulate various audio conditions
    println!("\n--- Processing Audio Frames ---");

    // 1. Silence
    println!("\n1. Processing silence...");
    let silence: Vec<i16> = vec![0; 320];
    for i in 0..5 {
        let (state, confidence) = vad.process(&silence).unwrap();
        println!(
            "   Frame {}: {:?} (confidence: {:.2})",
            i + 1,
            state,
            confidence
        );
    }

    // 2. Low background noise
    println!("\n2. Processing background noise...");
    for i in 0..10 {
        let noise: Vec<i16> = (0..320).map(|_| (rand::random() % 50) as i16).collect();
        let (state, confidence) = vad.process(&noise).unwrap();
        if i % 3 == 0 {
            println!(
                "   Frame {}: {:?} (confidence: {:.2}), Noise level: {:.1}",
                i + 1,
                state,
                confidence,
                vad.noise_level()
            );
        }
    }
    println!("   ✓ Adaptive threshold: {:.1}", vad.energy_threshold());

    // 3. Speech
    println!("\n3. Processing speech...");
    let speech: Vec<i16> = (0..320)
        .map(|i| ((i as f32 * 0.1).sin() * 2000.0) as i16)
        .collect();

    for i in 0..10 {
        let (state, confidence) = vad.process(&speech).unwrap();
        println!(
            "   Frame {}: {:?} (confidence: {:.2})",
            i + 1,
            state,
            confidence
        );
        if state == VadState::Speech {
            println!("   ✓ Speech detected!");
            break;
        }
    }

    // 4. Transition back to silence
    println!("\n4. Processing silence again...");
    for i in 0..15 {
        let (state, _) = vad.process(&silence).unwrap();
        if i % 5 == 0 {
            println!("   Frame {}: {:?}", i + 1, state);
        }
        if state == VadState::Silence {
            println!("   ✓ Silence detected after speech!");
            break;
        }
    }

    println!("\n=== Barge-in Detection Demo ===\n");

    // Reset VAD for barge-in demo
    vad.reset();
    let vad_config = VadConfig {
        sensitivity: 0.6,
        min_speech_duration_ms: 50,
        min_silence_duration_ms: 100,
        sample_rate: 16000,
        frame_size_ms: 20,
        energy_threshold: 100.0,
        zcr_threshold: 0.3,
    };
    let vad = VadDetector::new(vad_config);

    let bargein_config = BargeInConfig {
        enabled: true,
        cooldown_duration: Duration::from_millis(500),
        confidence_threshold: 0.6,
        min_user_speech_duration: Duration::from_millis(100),
    };

    let mut bargein = BargeInDetector::new(bargein_config, vad);
    println!("✓ Barge-in detector initialized");

    // Scenario 1: Normal conversation (no barge-in)
    println!("\n--- Scenario 1: Normal Turn-Taking ---");
    println!("User speaks...");
    for _ in 0..5 {
        bargein.process_audio(&speech).unwrap();
    }
    println!("✓ User speech detected, but AI not speaking - no barge-in");

    // Scenario 2: User interrupts AI
    println!("\n--- Scenario 2: User Interrupts AI ---");
    bargein.ai_started_speaking();
    println!("AI starts speaking...");
    println!("State: {:?}", bargein.state());

    std::thread::sleep(Duration::from_millis(100));

    println!("User starts speaking (interrupt)...");
    let mut frames_processed = 0;
    for _ in 0..20 {
        let (detected, vad_state, confidence) = bargein.process_audio(&speech).unwrap();

        if frames_processed % 5 == 0 {
            println!(
                "  Frame {}: VAD={:?}, Confidence={:.2}, Barge-in={}",
                frames_processed + 1,
                vad_state,
                confidence,
                if detected { "YES" } else { "no" }
            );
        }

        if detected {
            println!("\n🎯 BARGE-IN DETECTED!");
            println!("   State: {:?}", bargein.state());
            println!("   Interrupt count: {}", bargein.interrupt_count());
            println!("   → Would send interrupt signal to AI now");
            break;
        }

        frames_processed += 1;
        std::thread::sleep(Duration::from_millis(20));
    }

    // Scenario 3: Cooldown period
    println!("\n--- Scenario 3: Cooldown Period ---");
    println!("Immediately trying to detect barge-in again...");
    let (detected, _, _) = bargein.process_audio(&speech).unwrap();
    println!("  Barge-in detected: {} (blocked by cooldown)", detected);

    println!("\nWaiting for cooldown to expire...");
    std::thread::sleep(Duration::from_millis(600));

    bargein.ai_started_speaking();
    for _ in 0..10 {
        let (detected, _, _) = bargein.process_audio(&speech).unwrap();
        if detected {
            println!("  ✓ Barge-in detected after cooldown!");
            println!("  Interrupt count: {}", bargein.interrupt_count());
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    println!("\n=== Demo Complete ===");
    println!("\nKey Takeaways:");
    println!("• VAD adapts to background noise levels");
    println!("• Speech detection uses energy + zero-crossing rate");
    println!("• Barge-in only triggers when AI is actively speaking");
    println!("• Cooldown prevents rapid re-triggering");
    println!("• Minimum speech duration prevents false positives");
}

// Simple random number generator for demo
mod rand {
    static mut SEED: u32 = 12345;

    pub fn random() -> u32 {
        unsafe {
            SEED = SEED.wrapping_mul(1664525).wrapping_add(1013904223);
            SEED
        }
    }
}
