//! Emote and badge lookup tables with provider precedence.
use std::collections::HashMap;

use crate::net::{BadgeDef, EmoteDef, Provider};

#[derive(Default)]
pub struct Maps {
    /// name -> (url, provider, priority)
    pub emotes: HashMap<String, (String, Provider, u8)>,
    /// (set, version) -> (url, title)
    pub badges: HashMap<(String, String), (String, String)>,
}

/// Higher wins: channel sets beat global ones, and within a scope 7TV > BTTV > FFZ.
pub fn prio(provider: Provider, channel: bool) -> u8 {
    let base = match provider {
        Provider::SevenTv => 3,
        Provider::Bttv => 2,
        Provider::Ffz => 1,
    };
    if channel {
        base + 3
    } else {
        base
    }
}

impl Maps {
    pub fn add_emotes(&mut self, provider: Provider, channel: bool, list: Vec<EmoteDef>) {
        let pr = prio(provider, channel);
        for e in list {
            let better = self.emotes.get(&e.name).map(|x| pr > x.2).unwrap_or(true);
            if better {
                self.emotes.insert(e.name, (e.url, provider, pr));
            }
        }
    }

    pub fn add_badges(&mut self, list: Vec<BadgeDef>) {
        for b in list {
            self.badges.insert((b.set, b.version), (b.url, b.title));
        }
    }
}
