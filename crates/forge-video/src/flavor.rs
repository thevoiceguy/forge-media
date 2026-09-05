//! Flavors: what a receiver consumes, and sharing encoders between
//! receivers that consume the same thing.
//!
//! A participant subscribes to a flavor of a layout output; every
//! subscriber with the same flavor is served by one encoder. Ten identical
//! H.264 phones in a room cost one encode.

use crate::frame::Resolution;
use forge_core::VideoCodec;
use std::collections::BTreeMap;
use std::fmt;

/// One distinct encoder output.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Flavor {
    pub codec: VideoCodec,
    /// Codec profile / fmtp as negotiated, normalised (e.g. H.264
    /// `profile-level-id=42e01f;packetization-mode=1`); empty when the
    /// codec has none.
    pub profile: String,
    pub resolution: Resolution,
    pub fps: u32,
    /// Bitrate cap in kb/s.
    pub max_kbps: u32,
}

impl Flavor {
    pub fn new(
        codec: VideoCodec,
        profile: &str,
        resolution: Resolution,
        fps: u32,
        max_kbps: u32,
    ) -> Self {
        Self {
            codec,
            profile: normalise_profile(profile),
            resolution,
            fps,
            max_kbps,
        }
    }

    /// A flavor at the next rung down the ladder, or `None` at the
    /// bottom. Used when a subscriber's bandwidth falls far below the
    /// flavor's cap.
    pub fn step_down(&self) -> Option<Flavor> {
        let (res, kbps) = match self.resolution.height {
            h if h > 720 => (Resolution::new(1280, 720), 1200),
            h if h > 360 => (Resolution::new(640, 360), 500),
            h if h > 180 => (Resolution::new(320, 180), 200),
            _ => return None,
        };
        Some(Flavor {
            resolution: res,
            max_kbps: kbps.min(self.max_kbps),
            ..self.clone()
        })
    }
}

impl fmt::Display for Flavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{}p{}/{}kbps",
            self.codec, self.resolution.height, self.fps, self.max_kbps
        )?;
        if !self.profile.is_empty() {
            write!(f, "[{}]", self.profile)?;
        }
        Ok(())
    }
}

/// Lower-case, parameters sorted, whitespace dropped: two fmtp strings
/// that mean the same thing compare equal.
pub fn normalise_profile(p: &str) -> String {
    let mut parts: Vec<String> = p
        .split(';')
        .map(|kv| kv.trim().to_ascii_lowercase())
        .filter(|kv| !kv.is_empty())
        .collect();
    parts.sort();
    parts.join(";")
}

/// Subscribers per flavor. Keyed by subscriber id so a subscriber can
/// move between flavors without a double count.
#[derive(Debug, Default)]
pub struct FlavorTable {
    by_subscriber: BTreeMap<String, Flavor>,
    counts: BTreeMap<Flavor, usize>,
}

impl FlavorTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe `id` to `flavor`. Returns `true` when the flavor is new
    /// to the table (an encoder must be started).
    pub fn subscribe(&mut self, id: &str, flavor: Flavor) -> bool {
        self.unsubscribe(id);
        let count = self.counts.entry(flavor.clone()).or_insert(0);
        *count += 1;
        let new = *count == 1;
        self.by_subscriber.insert(id.to_string(), flavor);
        new
    }

    /// Drop `id`'s subscription. Returns the flavor that lost its last
    /// subscriber, if any (its encoder can be stopped).
    pub fn unsubscribe(&mut self, id: &str) -> Option<Flavor> {
        let flavor = self.by_subscriber.remove(id)?;
        let count = self.counts.get_mut(&flavor)?;
        *count -= 1;
        if *count == 0 {
            self.counts.remove(&flavor);
            Some(flavor)
        } else {
            None
        }
    }

    pub fn flavor_of(&self, id: &str) -> Option<&Flavor> {
        self.by_subscriber.get(id)
    }

    pub fn subscribers_of(&self, flavor: &Flavor) -> Vec<&str> {
        self.by_subscriber
            .iter()
            .filter(|(_, f)| *f == flavor)
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Every active flavor with its subscriber count.
    pub fn flavors(&self) -> impl Iterator<Item = (&Flavor, usize)> {
        self.counts.iter().map(|(f, n)| (f, *n))
    }

    pub fn encoder_count(&self) -> usize {
        self.counts.len()
    }

    pub fn subscriber_count(&self) -> usize {
        self.by_subscriber.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h264(h: u32) -> Flavor {
        Flavor::new(
            VideoCodec::H264,
            "packetization-mode=1; profile-level-id=42E01F",
            Resolution::new(h * 16 / 9, h),
            15,
            1200,
        )
    }

    #[test]
    fn profiles_normalise_so_equal_things_share() {
        let a = Flavor::new(
            VideoCodec::H264,
            "profile-level-id=42e01f;packetization-mode=1",
            Resolution::new(1280, 720),
            15,
            1200,
        );
        let b = h264(720);
        assert_eq!(a, b);
        assert_eq!(
            a.to_string(),
            "H264@720p15/1200kbps[packetization-mode=1;profile-level-id=42e01f]"
        );
        assert_eq!(normalise_profile(""), "");
    }

    #[test]
    fn table_shares_encoders_and_reports_first_and_last() {
        let mut t = FlavorTable::new();
        assert!(t.subscribe("p1", h264(720)));
        assert!(!t.subscribe("p2", h264(720)));
        assert!(t.subscribe("p3", h264(360)));
        assert_eq!(t.encoder_count(), 2);
        assert_eq!(t.subscriber_count(), 3);
        assert_eq!(t.subscribers_of(&h264(720)), vec!["p1", "p2"]);
        // Moving p2 down does not double count and starts nothing new.
        assert!(!t.subscribe("p2", h264(360)));
        assert_eq!(t.subscribers_of(&h264(720)), vec!["p1"]);
        assert_eq!(t.flavor_of("p2").unwrap().resolution.height, 360);
        // Last subscriber leaving returns the flavor.
        assert_eq!(t.unsubscribe("p1"), Some(h264(720)));
        assert_eq!(t.unsubscribe("p1"), None);
        assert_eq!(t.unsubscribe("p2"), None);
        assert_eq!(t.unsubscribe("p3"), Some(h264(360)));
        assert_eq!(t.encoder_count(), 0);
    }

    #[test]
    fn ladder_steps_down_to_the_floor() {
        let f = Flavor::new(VideoCodec::VP8, "", Resolution::new(1920, 1080), 30, 2500);
        let s1 = f.step_down().unwrap();
        assert_eq!((s1.resolution.height, s1.max_kbps), (720, 1200));
        let s2 = s1.step_down().unwrap();
        assert_eq!((s2.resolution.height, s2.max_kbps), (360, 500));
        let s3 = s2.step_down().unwrap();
        assert_eq!((s3.resolution.height, s3.max_kbps), (180, 200));
        assert!(s3.step_down().is_none());
        // A cap lower than the rung's default is kept.
        let cheap = Flavor::new(VideoCodec::VP8, "", Resolution::new(1280, 720), 30, 300);
        assert_eq!(cheap.step_down().unwrap().max_kbps, 300);
    }
}
