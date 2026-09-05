//! Native video codecs for forge-video, each behind a cargo feature so a
//! build without them still compiles and tests (with forge-video's raw
//! codec). Every binding implements [`VideoDecoder`] / [`VideoEncoder`]
//! on the host device and registers through [`register_all`].
//!
//! | Feature | Library | Codecs |
//! |---|---|---|
//! | `vpx` | libvpx (system) | VP8, VP9 encode + decode |
//! | `openh264` | OpenH264 (built from source) | H.264 encode + decode |
//! | `dav1d` | libdav1d (system) | AV1 decode |
//! | `svt-av1` | libsvtav1enc (system) | AV1 encode |
//!
//! Licensing and the measured cost of each are in FCP's
//! `docs/VIDEO_CONFERENCING.md` (§6, §9.4).

#![allow(clippy::needless_return)]

pub use forge_video::codec::{
    CodecError, CodecRegistry, EncoderSettings, VideoDecoder, VideoEncoder,
};

#[cfg(feature = "dav1d")]
pub mod dav1d_dec;
#[cfg(feature = "openh264")]
pub mod openh264_codec;
#[cfg(feature = "svt-av1")]
pub mod svt_av1;
#[cfg(feature = "vpx")]
pub mod vpx;

#[cfg(all(
    test,
    any(
        feature = "vpx",
        feature = "openh264",
        all(feature = "dav1d", feature = "svt-av1")
    )
))]
mod bench;
#[cfg(all(
    test,
    any(
        feature = "vpx",
        feature = "openh264",
        feature = "dav1d",
        feature = "svt-av1"
    )
))]
pub(crate) mod testsrc;

/// Register every binding this build includes.
pub fn register_all(registry: &mut CodecRegistry) {
    #[cfg(feature = "vpx")]
    vpx::register(registry);
    #[cfg(feature = "openh264")]
    openh264_codec::register(registry);
    #[cfg(feature = "dav1d")]
    dav1d_dec::register(registry);
    #[cfg(feature = "svt-av1")]
    svt_av1::register(registry);
    let _ = registry;
}

/// Names of the features compiled in, for logs and health endpoints.
pub fn enabled_backends() -> Vec<&'static str> {
    let mut v = Vec::new();
    if cfg!(feature = "vpx") {
        v.push("vpx");
    }
    if cfg!(feature = "openh264") {
        v.push("openh264");
    }
    if cfg!(feature = "dav1d") {
        v.push("dav1d");
    }
    if cfg!(feature = "svt-av1") {
        v.push("svt-av1");
    }
    v
}

/// A registry with the raw codec for every video codec plus every native
/// binding compiled in (natives replace raw where both exist).
pub fn default_registry() -> CodecRegistry {
    let mut r = forge_video::raw::raw_registry();
    register_all(&mut r);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_the_raw_codec_at_least() {
        let r = default_registry();
        assert_eq!(r.codecs_on(&forge_video::MediaDevice::Host).len(), 5);
        let _ = enabled_backends();
    }
}
