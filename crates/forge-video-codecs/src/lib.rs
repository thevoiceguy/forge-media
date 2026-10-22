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
//! | `hwaccel` | FFmpeg device contexts (`forge-video-hw`) | NVDEC decode, NVENC encode, on a GPU |
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
    // The GPU's codecs, on the default device when it opens; a node
    // that names another device registers it itself.
    #[cfg(feature = "hwaccel")]
    if let Some(caps) = forge_video_hw::register_default(registry) {
        tracing::info!(device = %caps.device, decoders = ?caps.decoders, encoders = ?caps.encoders, "hardware video codecs registered");
    }
    let _ = registry;
}

/// Open `device` as the backend a room is placed on, with its codecs
/// registered in `registry` on the same open device (design §15.7,
/// block 8b). `None` when the device does not open, or when this build
/// has no `hwaccel`: the node then stays on the host.
#[cfg(feature = "hwaccel")]
pub fn open_device(
    registry: &mut CodecRegistry,
    device: &forge_video::frame::MediaDevice,
) -> Option<std::sync::Arc<dyn forge_video::device::DeviceBackend>> {
    let backend = match forge_video_hw::HwBackend::open(device) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(%device, error = %e, "hardware video device did not open");
            return None;
        }
    };
    match backend.register(registry) {
        Ok(caps) => {
            tracing::info!(%device, decoders = ?caps.decoders, encoders = ?caps.encoders,
                           "hardware video device open");
        }
        Err(e) => {
            tracing::warn!(%device, error = %e, "hardware video codecs did not register");
            return None;
        }
    }
    Some(std::sync::Arc::new(backend))
}

/// [`open_device`] without the feature: nothing opens.
#[cfg(not(feature = "hwaccel"))]
pub fn open_device(
    _registry: &mut CodecRegistry,
    device: &forge_video::frame::MediaDevice,
) -> Option<std::sync::Arc<dyn forge_video::device::DeviceBackend>> {
    tracing::warn!(%device, "this build has no hardware video (the hwaccel feature is off)");
    None
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
    if cfg!(feature = "hwaccel") {
        v.push("hwaccel");
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
