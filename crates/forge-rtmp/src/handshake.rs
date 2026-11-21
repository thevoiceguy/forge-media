//! The simple handshake (RTMP 1.0 §5.2): version 3, then 1536 bytes each
//! way and their echoes. Every ingest takes it from a publisher.

use crate::RtmpError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const SIZE: usize = 1536;

/// Bytes that need not be secret, only unlikely to repeat.
fn filler(buf: &mut [u8]) {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    for b in buf.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
}

fn hello() -> Vec<u8> {
    let mut c1 = vec![0u8; SIZE];
    // Time and zero, then the filler.
    filler(&mut c1[8..]);
    c1
}

/// The client's side: C0 C1, then S0 S1 S2, then C2.
pub async fn client<S: AsyncRead + AsyncWrite + Unpin>(io: &mut S) -> Result<(), RtmpError> {
    let c1 = hello();
    let mut out = Vec::with_capacity(1 + SIZE);
    out.push(3);
    out.extend_from_slice(&c1);
    io.write_all(&out).await.map_err(RtmpError::from_io)?;
    let mut s0 = [0u8; 1];
    io.read_exact(&mut s0).await.map_err(RtmpError::from_io)?;
    if s0[0] != 3 {
        return Err(RtmpError::Protocol(format!(
            "server speaks RTMP version {}",
            s0[0]
        )));
    }
    let mut s1 = vec![0u8; SIZE];
    io.read_exact(&mut s1).await.map_err(RtmpError::from_io)?;
    let mut s2 = vec![0u8; SIZE];
    io.read_exact(&mut s2).await.map_err(RtmpError::from_io)?;
    io.write_all(&s1).await.map_err(RtmpError::from_io)?;
    io.flush().await.map_err(RtmpError::from_io)?;
    Ok(())
}

/// The server's side: C0 C1, then S0 S1 S2, then C2.
pub async fn server<S: AsyncRead + AsyncWrite + Unpin>(io: &mut S) -> Result<(), RtmpError> {
    let mut c0c1 = vec![0u8; 1 + SIZE];
    io.read_exact(&mut c0c1).await.map_err(RtmpError::from_io)?;
    if c0c1[0] != 3 {
        return Err(RtmpError::Protocol(format!(
            "client speaks RTMP version {}",
            c0c1[0]
        )));
    }
    let s1 = hello();
    let mut out = Vec::with_capacity(1 + 2 * SIZE);
    out.push(3);
    out.extend_from_slice(&s1);
    out.extend_from_slice(&c0c1[1..]);
    io.write_all(&out).await.map_err(RtmpError::from_io)?;
    let mut c2 = vec![0u8; SIZE];
    io.read_exact(&mut c2).await.map_err(RtmpError::from_io)?;
    Ok(())
}
