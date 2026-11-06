//! Port of src/renderer/chat/input/inventory.ts and the picker types of EmotePicker.tsx (PickerItem /
//! PickerSection): the `emoteInventory` builders (categorized picker sections + the flattened autocomplete
//! inventory) for Twitch, Rumble and Kick, as pure functions over the pane's already-filtered provider sets
//! (settings.sevenTv / bttv / ffz applied by the caller).
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use super::autocomplete::InvEntry;
use crate::chat::twitch::emotes::{provider_emote_url, ProviderEmote, ProviderScopedMap, SizeOpt, TwitchEmotesResp, TwitchPickerEmote};
use crate::chat::twitch::use_twitch_data::ProviderSets;

#[derive(Clone, Debug, PartialEq)]
pub struct PickerItem {
    pub key: String,
    pub name: String,
    pub img: String,
    pub tooltip: String,
    pub insert: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerProvider {
    SevenTv,
    Bttv,
    Ffz,
    Twitch,
    Rumble,
    Kick,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PickerSection {
    pub id: String,
    pub label: String,
    pub provider: PickerProvider,
    pub items: Vec<PickerItem>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inventory {
    pub sections: Vec<PickerSection>,
    pub inventory: Vec<InvEntry>,
}

/// An insertion-ordered name -> entry map (JS `Map` semantics: a later write replaces the value in place).
#[derive(Default)]
struct InvMap {
    order: Vec<String>,
    map: HashMap<String, InvEntry>,
}

impl InvMap {
    fn set(&mut self, e: InvEntry) {
        if !self.map.contains_key(&e.name) {
            self.order.push(e.name.clone());
        }
        self.map.insert(e.name.clone(), e);
    }
    fn into_vec(mut self) -> Vec<InvEntry> {
        self.order.into_iter().filter_map(|n| self.map.remove(&n)).collect()
    }
}

/// A provider map's values in a stable order (the JS Map kept insertion order; ours is a HashMap, sorted by name).
fn values(m: &HashMap<String, Arc<ProviderEmote>>) -> Vec<Arc<ProviderEmote>> {
    let mut v: Vec<Arc<ProviderEmote>> = m.values().cloned().collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    v
}

pub fn build_twitch_inventory(provider_sets: Option<&ProviderSets>, twitch_emotes: &TwitchEmotesResp, personal: &[Arc<ProviderEmote>]) -> Inventory {
    let mut sections: Vec<PickerSection> = Vec::new();
    let mut inv = InvMap::default();

    let mut add_section = |id: &str, label: &str, provider: PickerProvider, items: Vec<PickerItem>| {
        if !items.is_empty() {
            sections.push(PickerSection { id: id.into(), label: label.into(), provider, items });
        }
    };
    let add_inv = |inv: &mut InvMap, list: &[PickerItem], label: &str, channel_scope: bool| {
        for it in list {
            inv.set(InvEntry { name: it.name.clone(), img: it.img.clone(), label: label.into(), channel_scope, insert: None });
        }
    };
    let to_items = |list: &[Arc<ProviderEmote>], tag: &str| -> Vec<PickerItem> {
        list.iter()
            .map(|e| PickerItem { key: format!("{}:{}", provider_key(e), e.id), name: e.name.clone(), img: provider_emote_url(e, SizeOpt::Small), tooltip: format!("{} ({tag})", e.name), insert: None })
            .collect()
    };

    // Twitch's own emotes first: they are what most messages use.
    let twitch_item = |e: &TwitchPickerEmote| PickerItem { key: format!("tw:{}", e.id), name: e.name.clone(), img: e.url.clone(), tooltip: format!("{} (Twitch)", e.name), insert: None };
    let twitch_channel: Vec<PickerItem> = twitch_emotes.channel.iter().map(twitch_item).collect();
    let twitch_global: Vec<PickerItem> = twitch_emotes.global.iter().map(twitch_item).collect();
    add_section("twitch-channel", "Twitch Channel", PickerProvider::Twitch, twitch_channel.clone());
    add_section("twitch-global", "Twitch Global", PickerProvider::Twitch, twitch_global.clone());
    let twitch_other: Vec<PickerItem> = twitch_emotes.other.iter().map(twitch_item).collect();
    add_section("twitch-other", "Twitch Subscriptions", PickerProvider::Twitch, twitch_other.clone());

    let personal_items: Vec<PickerItem> = personal
        .iter()
        .map(|e| PickerItem { key: format!("p:{}", e.id), name: e.name.clone(), img: provider_emote_url(e, SizeOpt::Small), tooltip: format!("{} (7TV Personal)", e.name), insert: None })
        .collect();
    add_section("7tv-personal", "7TV Personal", PickerProvider::SevenTv, personal_items.clone());

    if let Some(ps) = provider_sets {
        let split = |m: &ProviderScopedMap, global: bool| -> (Vec<Arc<ProviderEmote>>, Vec<Arc<ProviderEmote>>) {
            let all = values(if global { &m.global } else { &m.channel });
            let (zw, normal): (Vec<_>, Vec<_>) = all.into_iter().partition(|e| e.zero_width);
            (normal, zw)
        };
        let (seven_channel, seven_channel_zw) = split(&ps.seven, false);
        let (seven_global, seven_global_zw) = split(&ps.seven, true);
        let bttv_channel = values(&ps.bttv.channel);
        let bttv_global = values(&ps.bttv.global);
        let ffz_channel = values(&ps.ffz.channel);
        let ffz_global = values(&ps.ffz.global);

        add_section("7tv-channel", "7TV Channel", PickerProvider::SevenTv, to_items(&seven_channel, "7TV"));
        let zw_all: Vec<Arc<ProviderEmote>> = seven_channel_zw.iter().chain(seven_global_zw.iter()).cloned().collect();
        add_section("7tv-zerowidth", "7TV Zero-Width", PickerProvider::SevenTv, to_items(&zw_all, "7TV zero-width"));
        add_section("7tv-global", "7TV Global", PickerProvider::SevenTv, to_items(&seven_global, "7TV"));
        add_section("bttv-channel", "BetterTTV Channel", PickerProvider::Bttv, to_items(&bttv_channel, "BetterTTV"));
        add_section("bttv-global", "BetterTTV Global", PickerProvider::Bttv, to_items(&bttv_global, "BetterTTV"));
        add_section("ffz-channel", "FrankerFaceZ Channel", PickerProvider::Ffz, to_items(&ffz_channel, "FrankerFaceZ"));
        add_section("ffz-global", "FrankerFaceZ Global", PickerProvider::Ffz, to_items(&ffz_global, "FrankerFaceZ"));

        // Lowest priority first so a later (higher priority) write wins the name, like mergeEmoteSets.
        add_inv(&mut inv, &to_items(&ffz_global, "FrankerFaceZ"), "FrankerFaceZ", false);
        add_inv(&mut inv, &to_items(&bttv_global, "BetterTTV"), "BetterTTV", false);
        add_inv(&mut inv, &to_items(&seven_global, "7TV"), "7TV", false);
        add_inv(&mut inv, &to_items(&seven_global_zw, "7TV zero-width"), "7TV zero-width", false);
        add_inv(&mut inv, &twitch_global, "Twitch", false);
        add_inv(&mut inv, &to_items(&ffz_channel, "FrankerFaceZ"), "FrankerFaceZ", true);
        add_inv(&mut inv, &to_items(&bttv_channel, "BetterTTV"), "BetterTTV", true);
        add_inv(&mut inv, &to_items(&seven_channel, "7TV"), "7TV", true);
        add_inv(&mut inv, &to_items(&seven_channel_zw, "7TV zero-width"), "7TV zero-width", true);
    } else {
        add_inv(&mut inv, &twitch_global, "Twitch", false);
    }
    add_inv(&mut inv, &twitch_other, "Twitch", true);
    add_inv(&mut inv, &twitch_channel, "Twitch", true);
    add_inv(&mut inv, &personal_items, "7TV Personal", true);

    Inventory { sections, inventory: inv.into_vec() }
}

fn provider_key(e: &ProviderEmote) -> &'static str {
    use crate::chat::twitch::emotes::EmoteProvider::*;
    match e.provider {
        SevenTv => "7tv",
        Bttv => "bttv",
        Ffz => "ffz",
    }
}

/// `names` is the Rumble emote map: name -> { file }.
pub fn build_rumble_inventory(emotes: &serde_json::Map<String, Value>) -> Inventory {
    if emotes.is_empty() {
        return Inventory::default();
    }
    let items: Vec<PickerItem> = emotes
        .iter()
        .map(|(name, e)| PickerItem {
            key: format!("rumble:{name}"),
            name: name.clone(),
            img: e.get("file").and_then(|f| f.as_str()).unwrap_or("").to_string(),
            tooltip: format!(":{name}:"),
            insert: Some(format!(":{name}:")),
        })
        .collect();
    Inventory {
        inventory: items.iter().map(|it| InvEntry { name: it.name.clone(), img: it.img.clone(), label: "Rumble".into(), channel_scope: true, insert: it.insert.clone() }).collect(),
        sections: vec![PickerSection { id: "rumble".into(), label: "Rumble".into(), provider: PickerProvider::Rumble, items }],
    }
}

pub fn kick_emote_url(id: &str) -> String {
    format!("https://files.kick.com/emotes/{id}/fullsize")
}

/// `emotes` is the Kick emote map: name -> { id, group, subsOnly }.
pub fn build_kick_inventory(emotes: &Value) -> Inventory {
    let Some(map) = emotes.as_object().filter(|m| !m.is_empty()) else { return Inventory::default() };
    let items: Vec<PickerItem> = map
        .iter()
        .map(|(name, e)| {
            let id = e.get("id").map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())).unwrap_or_default();
            PickerItem {
                key: format!("kick:{name}"),
                name: name.clone(),
                img: kick_emote_url(&id),
                tooltip: name.clone(),
                // Kick's own wire format, which is what its API expects back.
                insert: Some(format!("[emote:{id}:{name}]")),
            }
        })
        .collect();
    Inventory {
        inventory: items.iter().map(|it| InvEntry { name: it.name.clone(), img: it.img.clone(), label: "Kick".into(), channel_scope: true, insert: it.insert.clone() }).collect(),
        sections: vec![PickerSection { id: "kick".into(), label: "Kick".into(), provider: PickerProvider::Kick, items }],
    }
}

pub fn merge_inventories(list: &[Inventory]) -> Inventory {
    if list.len() == 1 {
        return list[0].clone();
    }
    let mut seen = std::collections::HashSet::new();
    let mut sections = Vec::new();
    let mut keys = std::collections::HashSet::new();
    let mut inventory = Vec::new();
    for inv in list {
        for s in &inv.sections {
            if seen.insert(s.id.clone()) {
                sections.push(s.clone());
            }
        }
        for e in &inv.inventory {
            if keys.insert(format!("{}\0{}", e.name, e.insert.clone().unwrap_or_default())) {
                inventory.push(e.clone());
            }
        }
    }
    Inventory { sections, inventory }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::twitch::emotes::EmoteProvider;
    use serde_json::json;

    fn pe(provider: EmoteProvider, id: &str, name: &str, zero_width: bool) -> Arc<ProviderEmote> {
        Arc::new(ProviderEmote { provider, id: id.into(), name: name.into(), zero_width, animated: false, urls: [Some(format!("https://cdn/{id}/1x")), Some(format!("https://cdn/{id}/2x")), None, None] })
    }
    fn scoped(global: Vec<Arc<ProviderEmote>>, channel: Vec<Arc<ProviderEmote>>) -> ProviderScopedMap {
        ProviderScopedMap { global: global.into_iter().map(|e| (e.name.clone(), e)).collect(), channel: channel.into_iter().map(|e| (e.name.clone(), e)).collect() }
    }
    fn tw(id: &str, name: &str) -> TwitchPickerEmote {
        TwitchPickerEmote { id: id.into(), name: name.into(), url: format!("https://tw/{id}") }
    }

    #[test]
    fn sections_come_in_the_pickers_order_and_empty_ones_are_dropped() {
        let sets = ProviderSets {
            seven: scoped(vec![pe(EmoteProvider::SevenTv, "g1", "GLOBAL7", false), pe(EmoteProvider::SevenTv, "z1", "ZW", true)], vec![pe(EmoteProvider::SevenTv, "c1", "CHAN7", false)]),
            bttv: scoped(vec![], vec![pe(EmoteProvider::Bttv, "b1", "BTTVCH", false)]),
            ffz: scoped(vec![], vec![]),
        };
        let twitch = TwitchEmotesResp { channel: vec![tw("1", "ChanSub")], global: vec![tw("25", "Kappa")], other: vec![] };
        let inv = build_twitch_inventory(Some(&sets), &twitch, &[pe(EmoteProvider::SevenTv, "p1", "MINE", false)]);
        let ids: Vec<&str> = inv.sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["twitch-channel", "twitch-global", "7tv-personal", "7tv-channel", "7tv-zerowidth", "7tv-global", "bttv-channel"]);
        let zw = inv.sections.iter().find(|s| s.id == "7tv-zerowidth").unwrap();
        assert_eq!(zw.items[0].tooltip, "ZW (7TV zero-width)");
        let kappa = &inv.sections[1].items[0];
        assert_eq!((kappa.key.as_str(), kappa.img.as_str(), kappa.tooltip.as_str()), ("tw:25", "https://tw/25", "Kappa (Twitch)"));
        // small density URL for provider emotes
        let c7 = inv.sections.iter().find(|s| s.id == "7tv-channel").unwrap();
        assert_eq!(c7.items[0].img, "https://cdn/c1/1x");
        assert_eq!(c7.items[0].key, "7tv:c1");
    }

    #[test]
    fn inventory_priority_lets_channel_and_personal_win_a_clashing_name() {
        let sets = ProviderSets {
            seven: scoped(vec![pe(EmoteProvider::SevenTv, "g", "Same", false)], vec![pe(EmoteProvider::SevenTv, "c", "Same", false)]),
            bttv: scoped(vec![pe(EmoteProvider::Bttv, "bg", "Same", false)], vec![]),
            ffz: scoped(vec![], vec![]),
        };
        let inv = build_twitch_inventory(Some(&sets), &TwitchEmotesResp::default(), &[]);
        let same: Vec<_> = inv.inventory.iter().filter(|e| e.name == "Same").collect();
        assert_eq!(same.len(), 1);
        assert!(same[0].channel_scope);
        assert_eq!(same[0].label, "7TV");
        let with_personal = build_twitch_inventory(Some(&sets), &TwitchEmotesResp::default(), &[pe(EmoteProvider::SevenTv, "p", "Same", false)]);
        let e = with_personal.inventory.iter().find(|e| e.name == "Same").unwrap();
        assert_eq!(e.label, "7TV Personal");
    }

    #[test]
    fn without_provider_sets_only_twitch_emotes_are_offered() {
        let twitch = TwitchEmotesResp { channel: vec![tw("1", "ChanSub")], global: vec![tw("25", "Kappa")], other: vec![tw("9", "Elsewhere")] };
        let inv = build_twitch_inventory(None, &twitch, &[]);
        assert_eq!(inv.sections.len(), 3);
        assert_eq!(inv.sections[2].label, "Twitch Subscriptions");
        let names: Vec<&str> = inv.inventory.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Kappa", "Elsewhere", "ChanSub"]);
        assert!(inv.inventory.iter().find(|e| e.name == "Kappa").map(|e| !e.channel_scope).unwrap());
    }

    #[test]
    fn rumble_and_kick_entries_insert_their_wire_text() {
        let rumble = build_rumble_inventory(json!({"pepe": {"file": "https://r/pepe.png"}}).as_object().unwrap());
        assert_eq!(rumble.sections[0].provider, PickerProvider::Rumble);
        assert_eq!(rumble.inventory[0].insert.as_deref(), Some(":pepe:"));
        assert_eq!(rumble.sections[0].items[0].tooltip, ":pepe:");
        let kick = build_kick_inventory(&json!({"LUL": {"id": "123", "group": "global"}}));
        assert_eq!(kick.inventory[0].insert.as_deref(), Some("[emote:123:LUL]"));
        assert_eq!(kick.sections[0].items[0].img, "https://files.kick.com/emotes/123/fullsize");
        assert_eq!(build_kick_inventory(&Value::Null), Inventory::default());
        assert_eq!(build_rumble_inventory(&Default::default()), Inventory::default());
    }

    #[test]
    fn merging_dedupes_sections_and_entries() {
        let kick = build_kick_inventory(&json!({"LUL": {"id": "1"}}));
        let rumble = build_rumble_inventory(json!({"LUL": {"file": "x"}}).as_object().unwrap());
        let merged = merge_inventories(&[kick.clone(), rumble, kick.clone()]);
        assert_eq!(merged.sections.len(), 2);
        // same name, different insert text: both stay
        assert_eq!(merged.inventory.len(), 2);
        assert_eq!(merge_inventories(&[kick.clone()]), kick);
    }
}
