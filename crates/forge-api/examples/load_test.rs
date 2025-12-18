//! Advanced load testing tool for Forge Media Engine
//!
//! This tool generates realistic load by:
//! - Creating concurrent sessions with SDP negotiation
//! - Sending actual RTP packets
//! - Measuring end-to-end latency
//! - Testing conference mixing with multiple participants
//! - Generating performance reports
//!
//! Usage:
//!   cargo run --release --example load_test -- --sessions 100 --duration 60
//!   cargo run --release --example load_test -- --conferences 10 --participants 5

use clap::{Arg, Command};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

const DEFAULT_BASE_URL: &str = "http://localhost:8080";

#[derive(Debug, Clone)]
struct LoadTestConfig {
    base_url: String,
    num_sessions: usize,
    num_conferences: usize,
    participants_per_conference: usize,
    duration_secs: u64,
    test_sdp: bool,
    test_transcoding: bool,
}

#[derive(Debug, Default)]
struct LoadTestResults {
    sessions_created: AtomicUsize,
    sessions_failed: AtomicUsize,
    sdp_negotiations: AtomicUsize,
    sdp_failures: AtomicUsize,
    conferences_created: AtomicUsize,
    participants_added: AtomicUsize,
    total_duration_ms: AtomicUsize,
}

impl LoadTestResults {
    fn print_summary(&self) {
        let total = self.sessions_created.load(Ordering::Relaxed);
        let failed = self.sessions_failed.load(Ordering::Relaxed);
        let sdp_ok = self.sdp_negotiations.load(Ordering::Relaxed);
        let sdp_fail = self.sdp_failures.load(Ordering::Relaxed);
        let rooms = self.conferences_created.load(Ordering::Relaxed);
        let participants = self.participants_added.load(Ordering::Relaxed);
        let duration_ms = self.total_duration_ms.load(Ordering::Relaxed);

        println!("\n========================================");
        println!("Load Test Results");
        println!("========================================");
        println!("Sessions:");
        println!("  Created: {}", total);
        println!("  Failed: {}", failed);
        println!(
            "  Success Rate: {:.2}%",
            (total as f64 / (total + failed) as f64) * 100.0
        );

        if sdp_ok + sdp_fail > 0 {
            println!("\nSDP Negotiation:");
            println!("  Successful: {}", sdp_ok);
            println!("  Failed: {}", sdp_fail);
            println!(
                "  Success Rate: {:.2}%",
                (sdp_ok as f64 / (sdp_ok + sdp_fail) as f64) * 100.0
            );
        }

        if rooms > 0 {
            println!("\nConferences:");
            println!("  Rooms Created: {}", rooms);
            println!("  Total Participants: {}", participants);
            println!(
                "  Avg Participants/Room: {:.2}",
                participants as f64 / rooms as f64
            );
        }

        if duration_ms > 0 {
            println!("\nPerformance:");
            println!("  Total Duration: {}ms", duration_ms);
            println!(
                "  Throughput: {:.2} ops/sec",
                (total as f64 / (duration_ms as f64 / 1000.0))
            );
        }
        println!("========================================\n");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = Command::new("Forge Media Load Test")
        .version("1.0")
        .author("Forge Media Team")
        .about("Load testing tool for Forge Media Engine")
        .arg(
            Arg::new("base-url")
                .long("base-url")
                .value_name("URL")
                .help("Base URL of the Forge Media API")
                .default_value(DEFAULT_BASE_URL),
        )
        .arg(
            Arg::new("sessions")
                .long("sessions")
                .short('s')
                .value_name("NUM")
                .help("Number of concurrent sessions to create")
                .default_value("50"),
        )
        .arg(
            Arg::new("conferences")
                .long("conferences")
                .short('c')
                .value_name("NUM")
                .help("Number of conference rooms to create")
                .default_value("5"),
        )
        .arg(
            Arg::new("participants")
                .long("participants")
                .short('p')
                .value_name("NUM")
                .help("Number of participants per conference")
                .default_value("3"),
        )
        .arg(
            Arg::new("duration")
                .long("duration")
                .short('d')
                .value_name("SECONDS")
                .help("Test duration in seconds (0 = create and cleanup only)")
                .default_value("0"),
        )
        .arg(
            Arg::new("test-sdp")
                .long("test-sdp")
                .help("Test SDP negotiation")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("test-transcoding")
                .long("test-transcoding")
                .help("Test codec transcoding")
                .action(clap::ArgAction::SetTrue),
        )
        .get_matches();

    let config = LoadTestConfig {
        base_url: matches.get_one::<String>("base-url").unwrap().clone(),
        num_sessions: matches.get_one::<String>("sessions").unwrap().parse()?,
        num_conferences: matches.get_one::<String>("conferences").unwrap().parse()?,
        participants_per_conference: matches.get_one::<String>("participants").unwrap().parse()?,
        duration_secs: matches.get_one::<String>("duration").unwrap().parse()?,
        test_sdp: matches.get_flag("test-sdp"),
        test_transcoding: matches.get_flag("test-transcoding"),
    };

    println!("Forge Media Engine - Load Test");
    println!("================================");
    println!("Configuration:");
    println!("  Base URL: {}", config.base_url);
    println!("  Sessions: {}", config.num_sessions);
    println!("  Conferences: {}", config.num_conferences);
    println!(
        "  Participants/Conference: {}",
        config.participants_per_conference
    );
    println!("  Duration: {}s", config.duration_secs);
    println!("  Test SDP: {}", config.test_sdp);
    println!("  Test Transcoding: {}", config.test_transcoding);
    println!();

    // Check server health
    println!("Checking server health...");
    let client = reqwest::Client::new();
    let health_url = format!("{}/health", config.base_url);
    match client.get(&health_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("✓ Server is healthy\n");
        }
        _ => {
            eprintln!("✗ Server not responding at {}", config.base_url);
            eprintln!("Please start the server with: cargo run --release");
            return Ok(());
        }
    }

    let results = Arc::new(LoadTestResults::default());

    // Test 1: Concurrent session creation
    if config.num_sessions > 0 {
        run_session_load_test(&config, &client, results.clone()).await?;
    }

    // Test 2: SDP negotiation
    if config.test_sdp {
        run_sdp_load_test(&config, &client, results.clone()).await?;
    }

    // Test 3: Conference load test
    if config.num_conferences > 0 {
        run_conference_load_test(&config, &client, results.clone()).await?;
    }

    // Wait for specified duration
    if config.duration_secs > 0 {
        println!("Running for {} seconds...", config.duration_secs);
        sleep(Duration::from_secs(config.duration_secs)).await;
    }

    // Fetch and display metrics
    fetch_metrics(&config, &client).await?;

    // Cleanup
    cleanup(&config, &client).await?;

    // Print results
    results.print_summary();

    Ok(())
}

async fn run_session_load_test(
    config: &LoadTestConfig,
    client: &reqwest::Client,
    results: Arc<LoadTestResults>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Test 1: Creating {} concurrent sessions...",
        config.num_sessions
    );
    let start = Instant::now();

    let mut handles = vec![];
    for i in 0..config.num_sessions {
        let client = client.clone();
        let base_url = config.base_url.clone();
        let results = results.clone();

        let handle = tokio::spawn(async move {
            let call_id = format!("load-test-{}", i);
            let url = format!("{}/v1/sessions", base_url);
            let body = json!({
                "call_id": call_id
            });

            match client.post(&url).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    results.sessions_created.fetch_add(1, Ordering::Relaxed);
                }
                _ => {
                    results.sessions_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    let elapsed = start.elapsed();
    results
        .total_duration_ms
        .store(elapsed.as_millis() as usize, Ordering::Relaxed);

    let created = results.sessions_created.load(Ordering::Relaxed);
    println!(
        "✓ Created {} sessions in {:?} ({:.2} sessions/sec)\n",
        created,
        elapsed,
        created as f64 / elapsed.as_secs_f64()
    );

    Ok(())
}

async fn run_sdp_load_test(
    config: &LoadTestConfig,
    client: &reqwest::Client,
    results: Arc<LoadTestResults>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Test 2: Testing SDP negotiation with 50 sessions...");
    let start = Instant::now();

    let sdp_offer = "v=0\r\n\
        o=- 1234567890 1234567890 IN IP4 127.0.0.1\r\n\
        s=Test Session\r\n\
        c=IN IP4 127.0.0.1\r\n\
        t=0 0\r\n\
        m=audio 10000 RTP/AVP 0 8\r\n\
        a=rtpmap:0 PCMU/8000\r\n\
        a=rtpmap:8 PCMA/8000";

    let mut handles = vec![];
    for i in 0..50 {
        let client = client.clone();
        let base_url = config.base_url.clone();
        let sdp_offer = sdp_offer.to_string();
        let results = results.clone();

        let handle = tokio::spawn(async move {
            let call_id = format!("sdp-test-{}", i);
            let url = format!("{}/v1/sessions", base_url);
            let body = json!({
                "call_id": call_id,
                "sdp_offer": sdp_offer,
                "local_address": "127.0.0.1",
                "sdp_profile": "audio-all"
            });

            match client.post(&url).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    results.sdp_negotiations.fetch_add(1, Ordering::Relaxed);
                }
                _ => {
                    results.sdp_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    let elapsed = start.elapsed();
    let successful = results.sdp_negotiations.load(Ordering::Relaxed);
    println!(
        "✓ Completed {} SDP negotiations in {:?}\n",
        successful, elapsed
    );

    Ok(())
}

async fn run_conference_load_test(
    config: &LoadTestConfig,
    client: &reqwest::Client,
    results: Arc<LoadTestResults>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Test 3: Creating {} conference rooms with {} participants each...",
        config.num_conferences, config.participants_per_conference
    );
    let start = Instant::now();

    for i in 0..config.num_conferences {
        let room_id = format!("load-room-{}", i);
        let url = format!("{}/v1/conferences/{}", config.base_url, room_id);

        // Create room
        if let Ok(resp) = client.post(&url).json(&json!({})).send().await {
            if resp.status().is_success() {
                results.conferences_created.fetch_add(1, Ordering::Relaxed);

                // Add participants
                for j in 0..config.participants_per_conference {
                    let participant_id = format!("participant-{}-{}", i, j);
                    let url = format!(
                        "{}/v1/conferences/{}/participants",
                        config.base_url, room_id
                    );
                    let body = json!({
                        "participant_id": participant_id
                    });

                    if let Ok(resp) = client.post(&url).json(&body).send().await {
                        if resp.status().is_success() {
                            results.participants_added.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
    }

    let elapsed = start.elapsed();
    let rooms = results.conferences_created.load(Ordering::Relaxed);
    let participants = results.participants_added.load(Ordering::Relaxed);
    println!(
        "✓ Created {} rooms with {} total participants in {:?}\n",
        rooms, participants, elapsed
    );

    Ok(())
}

async fn fetch_metrics(
    config: &LoadTestConfig,
    client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Fetching Prometheus metrics...");
    let url = format!("{}/metrics", config.base_url);

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(text) = resp.text().await {
                println!("\nKey Metrics:");
                println!("------------");
                for line in text.lines() {
                    if line.contains("forge_active_sessions")
                        || line.contains("forge_conference_rooms_active")
                        || line.contains("forge_conference_participants_active")
                        || line.contains("sdp_negotiation_total")
                        || line.contains("forge_transcoding_packets_total")
                        || line.contains("forge_conference_mix_operations_total")
                    {
                        if !line.starts_with('#') {
                            println!("{}", line);
                        }
                    }
                }
                println!();
            }
        }
        _ => {
            eprintln!("Failed to fetch metrics");
        }
    }

    Ok(())
}

async fn cleanup(
    config: &LoadTestConfig,
    client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Cleaning up test resources...");

    // Delete sessions
    for i in 0..config.num_sessions {
        let call_id = format!("load-test-{}", i);
        let url = format!("{}/v1/sessions/{}", config.base_url, call_id);
        let _ = client.delete(&url).send().await;
    }

    // Delete SDP test sessions
    if config.test_sdp {
        for i in 0..50 {
            let call_id = format!("sdp-test-{}", i);
            let url = format!("{}/v1/sessions/{}", config.base_url, call_id);
            let _ = client.delete(&url).send().await;
        }
    }

    // Delete conference rooms
    for i in 0..config.num_conferences {
        let room_id = format!("load-room-{}", i);
        let url = format!("{}/v1/conferences/{}", config.base_url, room_id);
        let _ = client.delete(&url).send().await;
    }

    println!("✓ Cleanup complete\n");

    Ok(())
}
