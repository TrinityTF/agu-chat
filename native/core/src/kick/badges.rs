//! Kick's own badge artwork. Kick draws its global badges as inline SVGs (there is no image URL for them), so
//! they are bundled here and addressed with `agu-asset:` URLs that the image cache reads from memory instead of
//! fetching. A channel's own subscriber art (a real URL on the badge) always wins over these.

/// URL prefix of bundled art.
pub const SCHEME: &str = "agu-asset:";

macro_rules! art {
    ($($name:literal),* $(,)?) => {
        /// (asset path, SVG source)
        const ASSETS: &[(&str, &str)] = &[$((concat!("kick-badges/", $name, ".svg"), include_str!(concat!("../../assets/kick-badges/", $name, ".svg")))),*];
    };
}

art!(
    "broadcaster", "verified", "staff", "global_moderator", "global_admin", "sidekick", "moderator", "vip", "og", "founder", "subscriber",
    "sub_gifter-1", "sub_gifter-25", "sub_gifter-50", "sub_gifter-100", "sub_gifter-200",
);

/// Sub gifter tiers that have their own art (gifts needed).
const GIFTER_TIERS: [u32; 5] = [1, 25, 50, 100, 200];

/// The bytes behind an `agu-asset:` URL, if it is one we bundle.
pub fn asset_bytes(url: &str) -> Option<&'static [u8]> {
    let path = url.strip_prefix(SCHEME)?;
    ASSETS.iter().find(|(p, _)| *p == path).map(|(_, s)| s.as_bytes())
}

/// The highest gifter tier at or below `count` gifts (the first tier covers 0 or unknown).
fn gifter_tier(count: f64) -> u32 {
    let n = if count.is_finite() && count > 0.0 { count as u32 } else { 0 };
    GIFTER_TIERS.iter().rev().copied().find(|t| *t <= n).unwrap_or(GIFTER_TIERS[0])
}

/// The picture for a Kick badge: its own URL (a channel's custom subscriber art) first, else the bundled art for
/// the type, else None (unknown types are drawn as a glyph chip by the frontend).
pub fn badge_url(kind: &str, count: f64, own: Option<&str>) -> Option<String> {
    if let Some(u) = own.filter(|u| !u.is_empty()) {
        return Some(u.to_string());
    }
    let name = match kind {
        "sub_gifter" => format!("sub_gifter-{}", gifter_tier(count)),
        "broadcaster" | "verified" | "staff" | "global_moderator" | "global_admin" | "sidekick" | "moderator" | "vip" | "og" | "founder" | "subscriber" => kind.to_string(),
        _ => return None,
    };
    Some(format!("{SCHEME}kick-badges/{name}.svg"))
}

/// Tooltip text for a Kick badge ("Moderator", "Subscriber (6 months)", "Sub gifter (25 gifts)").
pub fn badge_label(kind: &str, text: &str, count: f64) -> String {
    let n = if count.is_finite() && count > 0.0 { count as i64 } else { 0 };
    let plural = |unit: &str| if n == 1 { unit.to_string() } else { format!("{unit}s") };
    let pretty = match kind {
        "broadcaster" => "Broadcaster",
        "moderator" => "Moderator",
        "vip" => "VIP",
        "og" => "OG",
        "founder" => "Founder",
        "verified" => "Verified",
        "staff" => "Staff",
        "global_moderator" => "Global moderator",
        "global_admin" => "Global admin",
        "sidekick" => "Sidekick",
        _ => "",
    };
    match kind {
        "subscriber" if n > 0 => format!("Subscriber ({n} {})", plural("month")),
        "subscriber" => "Subscriber".to_string(),
        "sub_gifter" if n > 0 => format!("Sub gifter ({n} {})", plural("gift")),
        "sub_gifter" => "Sub gifter".to_string(),
        _ if !pretty.is_empty() => pretty.to_string(),
        _ if !text.is_empty() && n > 0 => format!("{text} ({n})"),
        _ if !text.is_empty() => text.to_string(),
        _ if n > 0 => format!("{kind} ({n})"),
        _ => kind.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_svg_rasterises_at_the_wanted_size() {
        for (path, _) in ASSETS {
            let url = format!("{SCHEME}{path}");
            let bytes = asset_bytes(&url).unwrap();
            let d = crate::images::decode(bytes, (40, 40), false).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!((d.fw, d.fh), (40, 40), "{path}");
            assert_eq!(d.frames.len(), 1);
            let (px, _) = &d.frames[0];
            assert_eq!(px.len(), 40 * 40 * 4);
            assert!(px.chunks_exact(4).any(|p| p[3] > 0), "{path} drew nothing");
        }
    }

    #[test]
    fn svg_stays_sharp_when_the_box_grows() {
        let bytes = asset_bytes("agu-asset:kick-badges/moderator.svg").unwrap();
        assert_eq!(crate::images::decode(bytes, (64, 64), false).unwrap().fw, 64);
        assert_eq!(crate::images::decode(bytes, (0, 0), false).unwrap().fw, 64);
        assert_eq!(crate::images::decode(bytes, (20, 20), false).unwrap().fw, 20);
    }

    #[test]
    fn unknown_assets_are_not_ours() {
        assert!(asset_bytes("https://x/y.png").is_none());
        assert!(asset_bytes("agu-asset:kick-badges/nope.svg").is_none());
    }

    #[test]
    fn type_maps_to_bundled_art() {
        assert_eq!(badge_url("moderator", 0.0, None).as_deref(), Some("agu-asset:kick-badges/moderator.svg"));
        assert_eq!(badge_url("vip", 0.0, Some("")).as_deref(), Some("agu-asset:kick-badges/vip.svg"));
        assert_eq!(badge_url("subscriber", 6.0, None).as_deref(), Some("agu-asset:kick-badges/subscriber.svg"));
        assert_eq!(badge_url("bot", 0.0, None), None);
        assert_eq!(badge_url("trainee", 0.0, None), None);
        for k in ["broadcaster", "verified", "staff", "global_moderator", "global_admin", "sidekick", "moderator", "vip", "og", "founder", "subscriber", "sub_gifter"] {
            assert!(asset_bytes(&badge_url(k, 3.0, None).unwrap()).is_some(), "{k}");
        }
    }

    #[test]
    fn gifter_tiers_use_the_highest_threshold_reached() {
        let t = |n: f64| badge_url("sub_gifter", n, None).unwrap();
        assert_eq!(t(0.0), "agu-asset:kick-badges/sub_gifter-1.svg");
        assert_eq!(t(1.0), "agu-asset:kick-badges/sub_gifter-1.svg");
        assert_eq!(t(24.0), "agu-asset:kick-badges/sub_gifter-1.svg");
        assert_eq!(t(25.0), "agu-asset:kick-badges/sub_gifter-25.svg");
        assert_eq!(t(49.0), "agu-asset:kick-badges/sub_gifter-25.svg");
        assert_eq!(t(50.0), "agu-asset:kick-badges/sub_gifter-50.svg");
        assert_eq!(t(100.0), "agu-asset:kick-badges/sub_gifter-100.svg");
        assert_eq!(t(199.0), "agu-asset:kick-badges/sub_gifter-100.svg");
        assert_eq!(t(5000.0), "agu-asset:kick-badges/sub_gifter-200.svg");
    }

    #[test]
    fn custom_art_wins_over_bundled() {
        assert_eq!(badge_url("subscriber", 6.0, Some("https://files.kick.com/b.png")).as_deref(), Some("https://files.kick.com/b.png"));
        assert_eq!(badge_url("bot", 0.0, Some("https://x/b.png")).as_deref(), Some("https://x/b.png"));
    }

    #[test]
    fn labels() {
        assert_eq!(badge_label("moderator", "Moderator", 0.0), "Moderator");
        assert_eq!(badge_label("vip", "", 0.0), "VIP");
        assert_eq!(badge_label("subscriber", "Subscriber", 1.0), "Subscriber (1 month)");
        assert_eq!(badge_label("subscriber", "", 6.0), "Subscriber (6 months)");
        assert_eq!(badge_label("sub_gifter", "Sub Gifter", 25.0), "Sub gifter (25 gifts)");
        assert_eq!(badge_label("bot", "", 0.0), "bot");
        assert_eq!(badge_label("trainee", "Trainee", 0.0), "Trainee");
    }
}
