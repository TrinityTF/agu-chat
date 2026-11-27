//! Which views (tabs) hold which sources. A view is one chat list made of one or more channels; a channel shown
//! in several views is subscribed once (reference counted here), and its frames are routed to every view that
//! holds it. This is only the bookkeeping: `session.rs` does the subscribing and applies frames to the stores.
use std::collections::HashMap;

use agu_chat_core::protocol::{SourceRef, SubId};
use agu_chat_model::store::source_key;

/// What the session must do about one source of a view that was just added.
#[derive(Clone, Debug, PartialEq)]
pub enum Added {
    /// nobody held it: subscribe for good
    New(SourceRef),
    /// another view already holds the live subscription: this view only needs the current state replayed
    Held(SourceRef),
}

impl Added {
    pub fn source(&self) -> &SourceRef {
        match self {
            Added::New(s) | Added::Held(s) => s,
        }
    }
}

/// What `set_view` changed.
#[derive(Debug, Default, PartialEq)]
pub struct Change {
    /// the view did not exist before
    pub created: bool,
    pub added: Vec<Added>,
    /// sources nobody holds any more: (source key, its live subscription to end)
    pub released: Vec<(String, Option<SubId>)>,
}

struct Held {
    source: SourceRef,
    refs: usize,
    sub: Option<SubId>,
}

#[derive(Default)]
pub struct Hub {
    views: HashMap<String, Vec<SourceRef>>,
    sources: HashMap<String, Held>,
}

fn dedup(channels: &[SourceRef]) -> Vec<SourceRef> {
    let mut seen = std::collections::HashSet::new();
    channels.iter().filter(|s| seen.insert(source_key(s))).cloned().collect()
}

impl Hub {
    /// Make view `vk` hold exactly `channels` (created when new), in that order.
    pub fn set_view(&mut self, vk: &str, channels: &[SourceRef]) -> Change {
        let want = dedup(channels);
        let created = !self.views.contains_key(vk);
        let old = self.views.get(vk).cloned().unwrap_or_default();
        let mut ch = Change { created, ..Default::default() };
        for s in &old {
            let sk = source_key(s);
            if want.iter().any(|w| source_key(w) == sk) {
                continue;
            }
            if let Some(h) = self.sources.get_mut(&sk) {
                h.refs -= 1;
                if h.refs == 0 {
                    let h = self.sources.remove(&sk).unwrap();
                    ch.released.push((sk, h.sub));
                }
            }
        }
        for s in &want {
            let sk = source_key(s);
            if old.iter().any(|o| source_key(o) == sk) {
                continue;
            }
            match self.sources.get_mut(&sk) {
                Some(h) => {
                    h.refs += 1;
                    ch.added.push(Added::Held(s.clone()));
                }
                None => {
                    self.sources.insert(sk, Held { source: s.clone(), refs: 1, sub: None });
                    ch.added.push(Added::New(s.clone()));
                }
            }
        }
        self.views.insert(vk.to_string(), want);
        ch
    }

    /// Drop a view; the sources nobody holds any more come back with their subscriptions.
    pub fn close_view(&mut self, vk: &str) -> Vec<(String, Option<SubId>)> {
        if !self.views.contains_key(vk) {
            return Vec::new();
        }
        let released = self.set_view(vk, &[]).released;
        self.views.remove(vk);
        released
    }

    /// The live subscription of a source (set once it answered).
    pub fn set_sub(&mut self, sk: &str, sub: SubId) {
        if let Some(h) = self.sources.get_mut(sk) {
            h.sub = Some(sub);
        }
    }

    pub fn views_of(&self, sk: &str) -> Vec<String> {
        self.views.iter().filter(|(_, v)| v.iter().any(|s| source_key(s) == sk)).map(|(k, _)| k.clone()).collect()
    }

    pub fn sources_of(&self, vk: &str) -> Vec<SourceRef> {
        self.views.get(vk).cloned().unwrap_or_default()
    }

    pub fn has_view(&self, vk: &str) -> bool {
        self.views.contains_key(vk)
    }

    pub fn source(&self, sk: &str) -> Option<SourceRef> {
        self.sources.get(sk).map(|h| h.source.clone())
    }

    pub fn is_held(&self, sk: &str) -> bool {
        self.sources.contains_key(sk)
    }

    /// Every channel any view holds: (source key, source).
    pub fn all_sources(&self) -> Vec<(String, SourceRef)> {
        self.sources.iter().map(|(k, h)| (k.clone(), h.source.clone())).collect()
    }

    pub fn view_ids(&self) -> Vec<String> {
        self.views.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_core::protocol::Platform;

    fn src(p: Platform, c: &str) -> SourceRef {
        SourceRef { platform: p, channel: c.into() }
    }

    #[test]
    fn a_channel_in_two_views_is_subscribed_once() {
        let mut h = Hub::default();
        let a = h.set_view("v1", &[src(Platform::Twitch, "agu")]);
        assert_eq!(a.added, vec![Added::New(src(Platform::Twitch, "agu"))]);
        h.set_sub("twitch:agu", "sub1".into());
        let b = h.set_view("v2", &[src(Platform::Twitch, "agu"), src(Platform::Kick, "agu")]);
        assert_eq!(b.added, vec![Added::Held(src(Platform::Twitch, "agu")), Added::New(src(Platform::Kick, "agu"))]);
        let mut v = h.views_of("twitch:agu");
        v.sort();
        assert_eq!(v, vec!["v1", "v2"]);
        assert_eq!(h.views_of("kick:agu"), vec!["v2"]);
    }

    #[test]
    fn closing_one_view_keeps_the_other_subscribed() {
        let mut h = Hub::default();
        h.set_view("v1", &[src(Platform::Twitch, "agu")]);
        h.set_sub("twitch:agu", "sub1".into());
        h.set_view("v2", &[src(Platform::Twitch, "agu")]);
        assert!(h.close_view("v1").is_empty(), "v2 still shows it");
        assert_eq!(h.views_of("twitch:agu"), vec!["v2"]);
        assert_eq!(h.close_view("v2"), vec![("twitch:agu".to_string(), Some("sub1".to_string()))]);
        assert!(!h.is_held("twitch:agu"));
        assert!(h.close_view("v2").is_empty());
    }

    #[test]
    fn changing_a_view_releases_only_what_it_dropped() {
        let mut h = Hub::default();
        h.set_view("v1", &[src(Platform::Twitch, "a"), src(Platform::Kick, "a")]);
        h.set_sub("kick:a", "k".into());
        let c = h.set_view("v1", &[src(Platform::Twitch, "a"), src(Platform::X, "a")]);
        assert!(!c.created);
        assert_eq!(c.added, vec![Added::New(src(Platform::X, "a"))]);
        assert_eq!(c.released, vec![("kick:a".to_string(), Some("k".to_string()))]);
        assert_eq!(h.sources_of("v1").len(), 2);
    }

    #[test]
    fn the_same_channel_twice_in_one_view_counts_once() {
        let mut h = Hub::default();
        let c = h.set_view("v1", &[src(Platform::Twitch, "agu"), src(Platform::Twitch, "agu")]);
        assert_eq!(c.added.len(), 1);
        assert_eq!(h.sources_of("v1").len(), 1);
    }
}
