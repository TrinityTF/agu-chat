//! The shared preview cache: one entry per link (by `LinkTarget::key`), least recently used ones dropped past
//! `MAX_ENTRIES`; one request per key however many rows ask; at most `MAX_ACTIVE` requests at a time; a failure is
//! remembered (and not asked again) until its retry time. The frontend drives it: `get` from the rows, `next_job`
//! to start requests, `finish` with each answer. The clock is the caller's (ms), so it is testable.
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::{LinkTarget, Preview};
use crate::images::Fail;

pub const MAX_ENTRIES: usize = 300;
pub const MAX_ACTIVE: usize = 4;
/// Links waiting for a request slot; the oldest are dropped past this (a fast-moving chat scrolls them away).
pub const MAX_QUEUED: usize = 64;
/// A failure is asked again after this: a network blip / timeout / 5xx / no Twitch login yet ...
pub const RETRY_TRANSIENT_MS: u64 = 60_000;
/// ... and a link with nothing to show (deleted video, private post, login wall).
pub const RETRY_PERMANENT_MS: u64 = 30 * 60_000;

#[derive(Clone, Debug)]
enum Slot {
    Queued,
    Loading,
    Ready(Arc<Preview>),
    Failed { retry_at: u64 },
}

struct Entry {
    slot: Slot,
    /// LRU stamp (a counter, bumped on every use)
    used: u64,
}

pub struct PreviewCache {
    map: HashMap<String, Entry>,
    queue: VecDeque<LinkTarget>,
    active: usize,
    tick: u64,
    enabled: bool,
}

impl Default for PreviewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewCache {
    pub fn new() -> Self {
        PreviewCache { map: HashMap::new(), queue: VecDeque::new(), active: 0, tick: 0, enabled: true }
    }

    /// Off: nothing new is queued or started, and what was waiting is dropped (the setting "Link previews").
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            for t in self.queue.drain(..) {
                self.map.remove(&t.key);
            }
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The preview when it is in; otherwise None, and the link is queued for a request unless one is already
    /// queued / running or it failed less than its retry time ago.
    pub fn get(&mut self, t: &LinkTarget, now_ms: u64) -> Option<Arc<Preview>> {
        self.tick += 1;
        let tick = self.tick;
        if let Some(e) = self.map.get_mut(&t.key) {
            e.used = tick;
            match &e.slot {
                Slot::Ready(p) => return Some(p.clone()),
                Slot::Queued | Slot::Loading => return None,
                Slot::Failed { retry_at } if *retry_at > now_ms => return None,
                Slot::Failed { .. } => {}
            }
        }
        if !self.enabled {
            return None;
        }
        self.map.insert(t.key.clone(), Entry { slot: Slot::Queued, used: tick });
        self.queue.push_back(t.clone());
        while self.queue.len() > MAX_QUEUED {
            if let Some(old) = self.queue.pop_front() {
                self.map.remove(&old.key);
            }
        }
        self.evict();
        None
    }

    /// Without queueing anything.
    pub fn peek(&self, key: &str) -> Option<Arc<Preview>> {
        match self.map.get(key).map(|e| &e.slot) {
            Some(Slot::Ready(p)) => Some(p.clone()),
            _ => None,
        }
    }

    /// The next link to request, when a slot is free (it then counts as running until `finish`).
    pub fn next_job(&mut self) -> Option<LinkTarget> {
        if !self.enabled || self.active >= MAX_ACTIVE {
            return None;
        }
        let t = self.queue.pop_front()?;
        if let Some(e) = self.map.get_mut(&t.key) {
            e.slot = Slot::Loading;
        } else {
            self.map.insert(t.key.clone(), Entry { slot: Slot::Loading, used: self.tick });
        }
        self.active += 1;
        Some(t)
    }

    /// A request ended. Returns whether there is a preview to show now.
    pub fn finish(&mut self, key: &str, res: Result<Preview, Fail>, now_ms: u64) -> bool {
        self.active = self.active.saturating_sub(1);
        let (slot, ok) = match res {
            Ok(p) => (Slot::Ready(Arc::new(p)), true),
            Err(kind) => (Slot::Failed { retry_at: now_ms + if kind == Fail::Transient { RETRY_TRANSIENT_MS } else { RETRY_PERMANENT_MS } }, false),
        };
        let tick = self.tick;
        self.map.entry(key.to_string()).or_insert(Entry { slot: Slot::Loading, used: tick }).slot = slot;
        self.evict();
        ok
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn running(&self) -> usize {
        self.active
    }

    /// Drop the least recently used finished entries past MAX_ENTRIES (queued / running ones stay).
    fn evict(&mut self) {
        while self.map.len() > MAX_ENTRIES {
            let victim = self.map.iter().filter(|(_, e)| matches!(e.slot, Slot::Ready(_) | Slot::Failed { .. })).min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    self.map.remove(&k);
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_preview::{match_url, Provider};

    fn t(id: &str) -> LinkTarget {
        match_url(&format!("https://streamable.com/{id}")).unwrap()
    }

    fn p(title: &str) -> Preview {
        Preview { provider: Provider::Streamable, title: title.into(), subtitle: String::new(), thumb: None }
    }

    #[test]
    fn one_request_per_link() {
        let mut c = PreviewCache::new();
        assert!(c.get(&t("aaa"), 0).is_none());
        assert!(c.get(&t("aaa"), 0).is_none());
        let job = c.next_job().unwrap();
        assert_eq!(job.key, "streamable:aaa");
        assert!(c.next_job().is_none(), "asked twice, requested once");
        // still running: asking again queues nothing
        assert!(c.get(&t("aaa"), 5).is_none());
        assert!(c.next_job().is_none());
        assert!(c.finish("streamable:aaa", Ok(p("A")), 10));
        assert_eq!(c.get(&t("aaa"), 11).unwrap().title, "A");
        assert_eq!(c.peek("streamable:aaa").unwrap().title, "A");
        assert!(c.next_job().is_none());
    }

    #[test]
    fn at_most_four_at_a_time() {
        let mut c = PreviewCache::new();
        for i in 0..6 {
            c.get(&t(&format!("id{i}")), 0);
        }
        let jobs: Vec<_> = std::iter::from_fn(|| c.next_job()).collect();
        assert_eq!(jobs.len(), MAX_ACTIVE);
        assert_eq!(c.running(), 4);
        c.finish(&jobs[0].key, Ok(p("x")), 1);
        assert!(c.next_job().is_some());
        assert!(c.next_job().is_none());
    }

    #[test]
    fn failures_wait_for_their_retry_time() {
        let mut c = PreviewCache::new();
        c.get(&t("bad"), 0);
        let j = c.next_job().unwrap();
        assert!(!c.finish(&j.key, Err(Fail::Permanent), 1_000));
        assert!(c.get(&t("bad"), 2_000).is_none());
        assert!(c.next_job().is_none(), "not asked again every frame");
        assert!(c.get(&t("bad"), 1_000 + RETRY_PERMANENT_MS + 1).is_none());
        assert!(c.next_job().is_some(), "asked again once the wait is over");

        c.get(&t("blip"), 0);
        let j = c.next_job().unwrap();
        c.finish(&j.key, Err(Fail::Transient), 0);
        c.get(&t("blip"), RETRY_TRANSIENT_MS - 1);
        assert!(c.next_job().is_none());
        c.get(&t("blip"), RETRY_TRANSIENT_MS + 1);
        assert_eq!(c.next_job().unwrap().key, "streamable:blip");
    }

    #[test]
    fn least_recently_used_entries_go_first() {
        let mut c = PreviewCache::new();
        for i in 0..MAX_ENTRIES {
            let k = t(&format!("vid{i}"));
            c.get(&k, 0);
            let j = c.next_job().unwrap();
            c.finish(&j.key, Ok(p("x")), 0);
        }
        assert_eq!(c.len(), MAX_ENTRIES);
        // vid0 is used again, so vid1 is now the oldest
        assert!(c.get(&t("vid0"), 0).is_some());
        c.get(&t("new"), 0);
        assert_eq!(c.len(), MAX_ENTRIES, "bounded");
        assert!(c.peek("streamable:vid0").is_some());
        assert!(c.peek("streamable:vid1").is_none());
    }

    #[test]
    fn off_fetches_nothing() {
        let mut c = PreviewCache::new();
        c.get(&t("aaa"), 0);
        c.set_enabled(false);
        assert!(c.next_job().is_none());
        assert!(c.get(&t("bbb"), 0).is_none());
        assert!(c.next_job().is_none());
        assert!(c.is_empty());
        c.set_enabled(true);
        c.get(&t("aaa"), 0);
        assert!(c.next_job().is_some());
    }

    #[test]
    fn the_queue_is_bounded() {
        let mut c = PreviewCache::new();
        for i in 0..(MAX_QUEUED + 10) {
            c.get(&t(&format!("que{i}")), 0);
        }
        // the oldest were dropped: the first job is the 11th link
        assert_eq!(c.next_job().unwrap().key, "streamable:que10");
    }
}
