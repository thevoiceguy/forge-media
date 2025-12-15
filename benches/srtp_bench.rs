//! SRTP Performance Benchmarks
//!
//! Benchmarks for SRTP encryption/decryption operations to ensure
//! performance targets are met:
//! - Key derivation latency
//! - Encryption throughput (packets/sec)
//! - Decryption throughput
//! - AES-CM vs AES-GCM comparison
//! - Target: <1ms overhead per packet

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use forge_rtp::srtp::{SrtpContext, SrtpKeyMaterial, SrtpProfile};

/// Generate a test RTP packet
fn generate_rtp_packet(payload_size: usize) -> Vec<u8> {
    let mut packet = Vec::new();

    // RTP header (12 bytes)
    packet.push(0x80); // V=2, P=0, X=0, CC=0
    packet.push(0x60); // M=0, PT=96 (dynamic)
    packet.extend_from_slice(&0x1234u16.to_be_bytes()); // Sequence number
    packet.extend_from_slice(&0x12345678u32.to_be_bytes()); // Timestamp
    packet.extend_from_slice(&0x87654321u32.to_be_bytes()); // SSRC

    // Payload
    packet.extend(vec![0x42; payload_size]);

    packet
}

/// Generate a test RTCP packet
fn generate_rtcp_packet(payload_size: usize) -> Vec<u8> {
    let mut packet = Vec::new();

    // RTCP header (8 bytes minimum)
    packet.push(0x80); // V=2, P=0, RC=0
    packet.push(200); // PT=200 (SR - Sender Report)
    packet.extend_from_slice(&0x0001u16.to_be_bytes()); // Length
    packet.extend_from_slice(&0x87654321u32.to_be_bytes()); // SSRC

    // Payload
    packet.extend(vec![0x42; payload_size]);

    packet
}

/// Benchmark key derivation
fn bench_key_derivation(c: &mut Criterion) {
    let mut group = c.benchmark_group("srtp_key_derivation");

    let profiles = vec![
        (SrtpProfile::Aes128CmHmacSha1_80, "AES-128-CM-SHA1-80"),
        (SrtpProfile::AeadAes128Gcm, "AEAD-AES-128-GCM"),
        (SrtpProfile::AeadAes256Gcm, "AEAD-AES-256-GCM"),
    ];

    for (profile, name) in profiles {
        group.bench_with_input(BenchmarkId::from_parameter(name), &profile, |b, profile| {
            let master_key = vec![0x42; profile.master_key_len()];
            let master_salt = vec![0x43; profile.master_salt_len()];

            b.iter(|| {
                // Key derivation happens during SrtpKeyMaterial::new()
                let key_material = SrtpKeyMaterial::new(
                    black_box(master_key.clone()),
                    black_box(master_salt.clone()),
                    black_box(*profile),
                )
                .unwrap();

                black_box(key_material);
            });
        });
    }

    group.finish();
}

/// Benchmark RTP encryption
fn bench_rtp_encryption(c: &mut Criterion) {
    let mut group = c.benchmark_group("srtp_rtp_encryption");

    let packet_sizes = vec![160, 320, 480, 960, 1200]; // Typical audio packet sizes
    let profiles = vec![
        (SrtpProfile::Aes128CmHmacSha1_80, "AES-128-CM-SHA1-80"),
        (SrtpProfile::AeadAes128Gcm, "AEAD-AES-128-GCM"),
        (SrtpProfile::AeadAes256Gcm, "AEAD-AES-256-GCM"),
    ];

    for (profile, profile_name) in profiles {
        for &size in &packet_sizes {
            let id = format!("{}/{}", profile_name, size);
            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(BenchmarkId::from_parameter(id), &(profile, size), |b, &(profile, size)| {
                let master_key = vec![0x42; profile.master_key_len()];
                let master_salt = vec![0x43; profile.master_salt_len()];
                let key_material = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

                let mut ctx = SrtpContext::new();
                ctx.set_local_key(key_material);

                let packet = generate_rtp_packet(size);

                b.iter(|| {
                    black_box(ctx.protect_rtp(black_box(&packet)).unwrap());
                });
            });
        }
    }

    group.finish();
}

/// Benchmark RTP decryption
fn bench_rtp_decryption(c: &mut Criterion) {
    let mut group = c.benchmark_group("srtp_rtp_decryption");

    let packet_sizes = vec![160, 320, 480, 960, 1200];
    let profiles = vec![
        (SrtpProfile::Aes128CmHmacSha1_80, "AES-128-CM-SHA1-80"),
        (SrtpProfile::AeadAes128Gcm, "AEAD-AES-128-GCM"),
        (SrtpProfile::AeadAes256Gcm, "AEAD-AES-256-GCM"),
    ];

    for (profile, profile_name) in profiles {
        for &size in &packet_sizes {
            let id = format!("{}/{}", profile_name, size);
            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(
                BenchmarkId::from_parameter(id),
                &(profile, size),
                |b, &(profile, size)| {
                    let master_key = vec![0x42; profile.master_key_len()];
                    let master_salt = vec![0x43; profile.master_salt_len()];
                    let key_material =
                        SrtpKeyMaterial::new(master_key.clone(), master_salt.clone(), profile)
                            .unwrap();

                    // Encrypt context
                    let mut encrypt_ctx = SrtpContext::new();
                    encrypt_ctx.set_local_key(key_material.clone());

                    // Decrypt context
                    let mut decrypt_ctx = SrtpContext::new();
                    decrypt_ctx.set_remote_key(key_material);

                    let packet = generate_rtp_packet(size);
                    let encrypted = encrypt_ctx.protect_rtp(&packet).unwrap();

                    b.iter(|| {
                        black_box(decrypt_ctx.unprotect_rtp(black_box(&encrypted)).unwrap());
                    });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark RTCP encryption
fn bench_rtcp_encryption(c: &mut Criterion) {
    let mut group = c.benchmark_group("srtp_rtcp_encryption");

    let packet_sizes = vec![64, 128, 256];
    let profiles = vec![
        (SrtpProfile::Aes128CmHmacSha1_80, "AES-128-CM-SHA1-80"),
        (SrtpProfile::AeadAes128Gcm, "AEAD-AES-128-GCM"),
        (SrtpProfile::AeadAes256Gcm, "AEAD-AES-256-GCM"),
    ];

    for (profile, profile_name) in profiles {
        for &size in &packet_sizes {
            let id = format!("{}/{}", profile_name, size);
            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(
                BenchmarkId::from_parameter(id),
                &(profile, size),
                |b, &(profile, size)| {
                    let master_key = vec![0x42; profile.master_key_len()];
                    let master_salt = vec![0x43; profile.master_salt_len()];
                    let key_material = SrtpKeyMaterial::new(master_key, master_salt, profile).unwrap();

                    let mut ctx = SrtpContext::new();
                    ctx.set_local_key(key_material);

                    let packet = generate_rtcp_packet(size);

                    b.iter(|| {
                        black_box(ctx.protect_rtcp(black_box(&packet)).unwrap());
                    });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark RTCP decryption
fn bench_rtcp_decryption(c: &mut Criterion) {
    let mut group = c.benchmark_group("srtp_rtcp_decryption");

    let packet_sizes = vec![64, 128, 256];
    let profiles = vec![
        (SrtpProfile::Aes128CmHmacSha1_80, "AES-128-CM-SHA1-80"),
        (SrtpProfile::AeadAes128Gcm, "AEAD-AES-128-GCM"),
        (SrtpProfile::AeadAes256Gcm, "AEAD-AES-256-GCM"),
    ];

    for (profile, profile_name) in profiles {
        for &size in &packet_sizes {
            let id = format!("{}/{}", profile_name, size);
            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(
                BenchmarkId::from_parameter(id),
                &(profile, size),
                |b, &(profile, size)| {
                    let master_key = vec![0x42; profile.master_key_len()];
                    let master_salt = vec![0x43; profile.master_salt_len()];
                    let key_material =
                        SrtpKeyMaterial::new(master_key.clone(), master_salt.clone(), profile)
                            .unwrap();

                    // Encrypt context
                    let mut encrypt_ctx = SrtpContext::new();
                    encrypt_ctx.set_local_key(key_material.clone());

                    // Decrypt context
                    let mut decrypt_ctx = SrtpContext::new();
                    decrypt_ctx.set_remote_key(key_material);

                    let packet = generate_rtcp_packet(size);
                    let encrypted = encrypt_ctx.protect_rtcp(&packet).unwrap();

                    b.iter(|| {
                        black_box(decrypt_ctx.unprotect_rtcp(black_box(&encrypted)).unwrap());
                    });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark full roundtrip (encrypt + decrypt)
fn bench_rtp_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("srtp_rtp_roundtrip");

    let packet_sizes = vec![160, 320, 480, 960, 1200];
    let profiles = vec![
        (SrtpProfile::Aes128CmHmacSha1_80, "AES-128-CM-SHA1-80"),
        (SrtpProfile::AeadAes128Gcm, "AEAD-AES-128-GCM"),
        (SrtpProfile::AeadAes256Gcm, "AEAD-AES-256-GCM"),
    ];

    for (profile, profile_name) in profiles {
        for &size in &packet_sizes {
            let id = format!("{}/{}", profile_name, size);
            group.throughput(Throughput::Bytes(size as u64 * 2)); // Count both encrypt and decrypt
            group.bench_with_input(
                BenchmarkId::from_parameter(id),
                &(profile, size),
                |b, &(profile, size)| {
                    let master_key = vec![0x42; profile.master_key_len()];
                    let master_salt = vec![0x43; profile.master_salt_len()];
                    let key_material =
                        SrtpKeyMaterial::new(master_key.clone(), master_salt.clone(), profile)
                            .unwrap();

                    // Encrypt context
                    let mut encrypt_ctx = SrtpContext::new();
                    encrypt_ctx.set_local_key(key_material.clone());

                    // Decrypt context
                    let mut decrypt_ctx = SrtpContext::new();
                    decrypt_ctx.set_remote_key(key_material);

                    let packet = generate_rtp_packet(size);

                    b.iter(|| {
                        let encrypted = black_box(encrypt_ctx.protect_rtp(black_box(&packet)).unwrap());
                        let _decrypted = black_box(decrypt_ctx.unprotect_rtp(black_box(&encrypted)).unwrap());
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_key_derivation,
    bench_rtp_encryption,
    bench_rtp_decryption,
    bench_rtcp_encryption,
    bench_rtcp_decryption,
    bench_rtp_roundtrip
);
criterion_main!(benches);
