//! Who downloads next: a small scheduler in place of a FIFO semaphore. In fast chat the images of the newest rows
//! are wanted first, and the ones of rows that scrolled away may not be wanted at all any more, so:
//! - the most recently asked-for pending image goes next (asked within the same second: the newest request first);
//!   the UI asks for an image every frame it is drawn, so what is on screen stays at the front;
//! - a request nobody asked for in `STALE_MS` is dropped from the queue (asked for again, it is queued again);
//! - at most `MAX_TOTAL` downloads at once, and at most `MAX_PER_HOST` per host, so one slow CDN cannot take every
//!   slot from the others.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::oneshot;

pub const MAX_TOTAL: usize = 12;
pub const MAX_PER_HOST: usize = 6;
pub const STALE_MS: u64 = 30_000;
/// Requests asked for within the same bucket count as equally recent (then the newest request wins).
const ASKED_BUCKET_MS: u64 = 1_000;

pub fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.to_string())).unwrap_or_default()
}

struct Pending {
    host: String,
    seq: u64,
}

/// The queue itself, clocked by the caller (ms).
pub struct Queue {
    pending: HashMap<String, Pending>,
    /// When each URL that is queued, running or waiting to retry was last asked for.
    asked: HashMap<String, u64>,
    running: usize,
    per_host: HashMap<String, usize>,
    seq: u64,
    pub max_total: usize,
    pub max_per_host: usize,
    pub stale_ms: u64,
}

impl Default for Queue {
    fn default() -> Self {
        Queue { pending: HashMap::new(), asked: HashMap::new(), running: 0, per_host: HashMap::new(), seq: 0, max_total: MAX_TOTAL, max_per_host: MAX_PER_HOST, stale_ms: STALE_MS }
    }
}

impl Queue {
    /// Queues `url` (a retry keeps the time it was last asked for).
    pub fn push(&mut self, url: &str, now: u64) {
        self.seq += 1;
        self.asked.entry(url.to_string()).or_insert(now);
        self.pending.insert(url.to_string(), Pending { host: host_of(url), seq: self.seq });
    }

    /// `url` was asked for (drawn) at `now`.
    pub fn touch(&mut self, url: &str, now: u64) {
        if let Some(a) = self.asked.get_mut(url) {
            *a = (*a).max(now);
        }
    }

    /// `url` is done (loaded, failed for good or dropped): its next request starts fresh.
    pub fn forget(&mut self, url: &str) {
        self.asked.remove(url);
        self.pending.remove(url);
    }

    /// Takes a pending request out without running it (its waiter is gone).
    pub fn cancel(&mut self, url: &str) {
        self.pending.remove(url);
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn running(&self) -> usize {
        self.running
    }

    /// Starts what may start now. Returns (started, dropped as stale); every started one must be `finish`ed.
    pub fn dispatch(&mut self, now: u64) -> (Vec<String>, Vec<String>) {
        let stale: Vec<String> = self.pending.keys().filter(|u| now.saturating_sub(self.asked.get(*u).copied().unwrap_or(now)) > self.stale_ms).cloned().collect();
        for u in &stale {
            self.forget(u);
        }
        let mut started = Vec::new();
        while self.running < self.max_total {
            let next = self
                .pending
                .iter()
                .filter(|(_, p)| self.per_host.get(&p.host).copied().unwrap_or(0) < self.max_per_host)
                .max_by_key(|(u, p)| (self.asked.get(*u).copied().unwrap_or(0) / ASKED_BUCKET_MS, p.seq))
                .map(|(u, _)| u.clone());
            let Some(u) = next else { break };
            let p = self.pending.remove(&u).unwrap();
            self.running += 1;
            *self.per_host.entry(p.host).or_insert(0) += 1;
            started.push(u);
        }
        (started, stale)
    }

    /// A started download of `url` gave its slot back.
    pub fn finish(&mut self, url: &str) {
        self.running = self.running.saturating_sub(1);
        let h = host_of(url);
        if let Some(n) = self.per_host.get_mut(&h) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.per_host.remove(&h);
            }
        }
    }
}

/// The queue for async tasks: `acquire` waits for a slot (None = dropped as stale), the slot frees on drop.
pub struct Sched {
    inner: Mutex<(Queue, HashMap<String, oneshot::Sender<bool>>)>,
    start: Instant,
}

/// A download slot; given back when dropped.
pub struct Slot {
    sched: Arc<Sched>,
    url: String,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut g = self.sched.inner.lock().unwrap();
        g.0.finish(&self.url);
        self.sched.dispatch_locked(&mut *g);
    }
}

impl Sched {
    pub fn new(q: Queue) -> Arc<Sched> {
        Arc::new(Sched { inner: Mutex::new((q, HashMap::new())), start: Instant::now() })
    }

    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn dispatch_locked(&self, g: &mut (Queue, HashMap<String, oneshot::Sender<bool>>)) {
        loop {
            let (started, dropped) = g.0.dispatch(self.now());
            for u in dropped {
                if let Some(tx) = g.1.remove(&u) {
                    let _ = tx.send(false);
                }
            }
            let mut lost = false;
            for u in started {
                let ok = g.1.remove(&u).map(|tx| tx.send(true).is_ok()).unwrap_or(false);
                if !ok {
                    // the waiting task is gone: the slot is free again
                    g.0.finish(&u);
                    lost = true;
                }
            }
            if !lost {
                break;
            }
        }
    }

    /// Waits for a download slot for `url`; None when the request went stale in the queue.
    pub async fn acquire(self: &Arc<Self>, url: &str) -> Option<Slot> {
        let (tx, rx) = oneshot::channel();
        {
            let mut g = self.inner.lock().unwrap();
            let now = self.now();
            g.0.push(url, now);
            // a second waiter for the same URL replaces the first (which then sees None)
            g.1.insert(url.to_string(), tx);
            self.dispatch_locked(&mut *g);
        }
        match rx.await {
            Ok(true) => Some(Slot { sched: self.clone(), url: url.to_string() }),
            _ => None,
        }
    }

    pub fn touch(&self, url: &str) {
        let now = self.now();
        self.inner.lock().unwrap().0.touch(url, now);
    }

    pub fn forget(&self, url: &str) {
        let mut g = self.inner.lock().unwrap();
        g.0.forget(url);
        g.1.remove(url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(max_total: usize) -> Queue {
        Queue { max_total, ..Queue::default() }
    }

    fn u(n: &str) -> String {
        format!("https://cdn.example/{n}")
    }

    #[test]
    fn the_newest_request_goes_first() {
        let mut q = q(1);
        q.push(&u("a"), 0);
        assert_eq!(q.dispatch(0).0, vec![u("a")]);
        for n in ["b", "c", "d"] {
            q.push(&u(n), 10);
        }
        assert!(q.dispatch(10).0.is_empty(), "the slot is taken");
        let mut order = Vec::new();
        let mut running = u("a");
        for t in 0..3 {
            q.finish(&running);
            let s = q.dispatch(20 + t).0;
            assert_eq!(s.len(), 1);
            running = s[0].clone();
            order.push(running.clone());
        }
        assert_eq!(order, vec![u("d"), u("c"), u("b")]);
    }

    #[test]
    fn what_is_still_drawn_goes_before_newer_requests_that_scrolled_away() {
        let mut q = q(1);
        q.push(&u("busy"), 0);
        q.dispatch(0);
        q.push(&u("old"), 100);
        q.push(&u("new"), 200);
        // "old" is still on screen two seconds later, "new" is not
        q.touch(&u("old"), 2_200);
        q.finish(&u("busy"));
        assert_eq!(q.dispatch(2_300).0, vec![u("old")]);
    }

    #[test]
    fn requests_not_asked_for_in_a_while_are_dropped() {
        let mut q = q(1);
        q.push(&u("busy"), 0);
        q.dispatch(0);
        q.push(&u("gone"), 0);
        q.push(&u("seen"), 0);
        q.touch(&u("seen"), 25_000);
        q.finish(&u("busy"));
        let (started, dropped) = q.dispatch(STALE_MS + 1_000);
        assert_eq!(dropped, vec![u("gone")]);
        assert_eq!(started, vec![u("seen")]);
        assert_eq!(q.pending_len(), 0);
        // asked for again: queued again, fresh
        q.finish(&u("seen"));
        q.push(&u("gone"), 40_000);
        assert_eq!(q.dispatch(40_000).0, vec![u("gone")]);
    }

    #[test]
    fn a_retry_keeps_its_last_asked_time() {
        let mut q = q(2);
        q.push(&u("r"), 0);
        q.dispatch(0);
        q.finish(&u("r"));
        // backing off; re-queued much later without being drawn meanwhile: stale
        q.push(&u("r"), 40_000);
        let (started, dropped) = q.dispatch(40_000);
        assert!(started.is_empty());
        assert_eq!(dropped, vec![u("r")]);
    }

    #[test]
    fn one_host_cannot_take_every_slot() {
        let mut q = Queue { max_total: 4, max_per_host: 2, ..Queue::default() };
        for i in 0..4 {
            q.push(&format!("https://slow.cdn/{i}"), 0);
        }
        q.push("https://fast.cdn/x", 0);
        let started = q.dispatch(0).0;
        assert_eq!(started.len(), 3);
        assert_eq!(started.iter().filter(|s| s.starts_with("https://slow.cdn/")).count(), 2);
        assert!(started.contains(&"https://fast.cdn/x".to_string()));
        assert_eq!(q.running(), 3);
        q.finish(&started[0]);
        assert_eq!(q.dispatch(0).0.len(), if started[0].contains("slow") { 1 } else { 0 });
    }

    #[tokio::test]
    async fn slots_are_handed_out_newest_first_and_freed_on_drop() {
        let s = Sched::new(Queue { max_total: 1, ..Queue::default() });
        let first = s.acquire(&u("a")).await.expect("a free slot");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for n in ["b", "c"] {
            let (s, tx) = (s.clone(), tx.clone());
            tokio::spawn(async move {
                let slot = s.acquire(&u(n)).await;
                let _ = tx.send((n, slot.is_some()));
                drop(slot);
            });
            // the second waiter is queued after the first
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        drop(first);
        assert_eq!(rx.recv().await, Some(("c", true)));
        assert_eq!(rx.recv().await, Some(("b", true)));
    }
}
