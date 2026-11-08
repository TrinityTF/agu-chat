//! Port of src/renderer/layout/tree.ts.
//! Pure operations on the SplitNode tree (see protocol.rs). Nothing here touches the UI.
use std::sync::atomic::{AtomicU64, Ordering};

use agu_chat_core::protocol::{Layout, Platform, SourceRef, SplitNode, Tab};
use agu_chat_core::twitch::whisper;
use serde_json::Value;

/// `Extract<SplitNode, { kind: "leaf" }>`: a leaf's fields, as a value type that converts to/from `SplitNode`.
#[derive(Clone, Debug, PartialEq)]
pub struct Leaf {
    pub id: String,
    pub sources: Vec<SourceRef>,
    pub mod_mode: bool,
}

impl From<Leaf> for SplitNode {
    fn from(l: Leaf) -> Self {
        SplitNode::Leaf { id: l.id, sources: l.sources, mod_mode: l.mod_mode }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// "row" | "col" (the container kinds)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Row,
    Col,
}

pub fn node_id(n: &SplitNode) -> &str {
    match n {
        SplitNode::Leaf { id, .. } | SplitNode::Row { id, .. } | SplitNode::Col { id, .. } => id,
    }
}

/// The container kind of a node, `None` for a leaf.
pub fn axis_of(n: &SplitNode) -> Option<Axis> {
    match n {
        SplitNode::Leaf { .. } => None,
        SplitNode::Row { .. } => Some(Axis::Row),
        SplitNode::Col { .. } => Some(Axis::Col),
    }
}

fn container(axis: Axis, id: String, children: Vec<SplitNode>, sizes: Vec<f32>) -> SplitNode {
    match axis {
        Axis::Row => SplitNode::Row { id, children, sizes },
        Axis::Col => SplitNode::Col { id, children, sizes },
    }
}

pub fn uid() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    // splitmix64 over time + a counter, so two ids in the same instant differ
    let mut z = nanos.wrapping_add(COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut s = String::new();
    for _ in 0..8 {
        s.push(alphabet[(z % 36) as usize] as char);
        z /= 36;
    }
    s
}

pub fn new_leaf(sources: Vec<SourceRef>) -> Leaf {
    Leaf { id: uid(), sources, mod_mode: false }
}

pub fn new_tab(sources: Option<Vec<SourceRef>>) -> Tab {
    Tab {
        id: uid(),
        title: String::new(),
        root: sources.filter(|s| !s.is_empty()).map(|s| new_leaf(s).into()),
    }
}

pub fn leaves(node: Option<&SplitNode>) -> Vec<Leaf> {
    fn walk(n: &SplitNode, out: &mut Vec<Leaf>) {
        match n {
            SplitNode::Leaf { id, sources, mod_mode } => out.push(Leaf { id: id.clone(), sources: sources.clone(), mod_mode: *mod_mode }),
            SplitNode::Row { children, .. } | SplitNode::Col { children, .. } => children.iter().for_each(|c| walk(c, out)),
        }
    }
    let mut out = Vec::new();
    if let Some(n) = node {
        walk(n, &mut out);
    }
    out
}

pub fn find_leaf(node: Option<&SplitNode>, id: &str) -> Option<Leaf> {
    leaves(node).into_iter().find(|l| l.id == id)
}

pub fn source_key(s: &SourceRef) -> String {
    format!("{}:{}", s.platform.as_str(), s.channel)
}

/// Pulls the query value `v` and the last path segment out of a URL the way `new URL` would, for `short_name`.
fn parse_url(raw: &str) -> Option<(String, Option<String>, Vec<String>)> {
    let s = if let Some(rest) = raw.strip_prefix("https://").or_else(|| raw.strip_prefix("http://")) {
        rest.to_string()
    } else if raw.len() >= 8 && raw[..8].eq_ignore_ascii_case("https://") {
        raw[8..].to_string()
    } else if raw.len() >= 7 && raw[..7].eq_ignore_ascii_case("http://") {
        raw[7..].to_string()
    } else {
        raw.to_string()
    };
    let s = s.split('#').next().unwrap_or("");
    let (before_q, query) = match s.split_once('?') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let (host, path) = match before_q.split_once('/') {
        Some((h, p)) => (h, p),
        None => (before_q, ""),
    };
    if host.is_empty() {
        return None;
    }
    let v = query.and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("v=").map(|x| x.to_string())));
    let parts = path.split('/').filter(|p| !p.is_empty()).map(|p| p.to_string()).collect();
    Some((host.to_string(), v, parts))
}

fn looks_like_url(c: &str) -> bool {
    let l = c.to_lowercase();
    if l.starts_with("http://") || l.starts_with("https://") {
        return true;
    }
    // /^(www\.)?[a-z0-9-]+\.(com|tv|lol)\//i
    let l = l.strip_prefix("www.").unwrap_or(&l);
    if let Some(slash) = l.find('/') {
        let host = &l[..slash];
        if let Some((name, tld)) = host.rsplit_once('.') {
            return !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && matches!(tld, "com" | "tv" | "lol");
        }
    }
    false
}

/// A Twitch whisper conversation (a reserved channel, see core's twitch/whisper.rs).
pub fn is_whisper(s: &SourceRef) -> bool {
    s.platform == Platform::Twitch && whisper::login_of(&s.channel).is_some()
}

/// `is_whisper` for a source key (`twitch:whisper:bob`).
pub fn is_whisper_key(sk: &str) -> bool {
    sk.strip_prefix("twitch:").map(|c| whisper::login_of(c).is_some()).unwrap_or(false)
}

/// Short display name of a source: a URL shows its last meaningful part, a whisper conversation the other user.
pub fn short_name(s: &SourceRef) -> String {
    if let Some(login) = Some(s).filter(|s| s.platform == Platform::Twitch).and_then(|s| whisper::login_of(&s.channel)) {
        return login;
    }
    let c = s.channel.trim();
    if looks_like_url(c) {
        if let Some((host, v, parts)) = parse_url(c) {
            return v.filter(|v| !v.is_empty()).or_else(|| parts.last().cloned()).unwrap_or(host);
        }
    }
    c.to_string()
}

/// A tab that holds nothing but whisper conversations.
pub fn whisper_only(tab: &Tab) -> bool {
    let all: Vec<SourceRef> = leaves(tab.root.as_ref()).into_iter().flat_map(|l| l.sources).collect();
    !all.is_empty() && all.iter().all(is_whisper)
}

/// Every channel in the tab, once each ("EsfandTV" and "@esfandtv" count as one).
pub fn auto_title(tab: &Tab) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut names: Vec<String> = Vec::new();
    for n in leaves(tab.root.as_ref()).iter().flat_map(|l| l.sources.iter().map(short_name)) {
        let k = n.trim_start_matches('@').to_lowercase();
        if !seen.insert(k) {
            continue;
        }
        names.push(n);
    }
    if names.is_empty() {
        "New tab".into()
    } else {
        names.join(", ")
    }
}

/// A tab with an empty title shows the channels it contains: the first, then "…" when there are more (the full list is its tooltip).
pub fn tab_title(tab: &Tab, full: bool) -> String {
    let own = tab.title.trim();
    if !own.is_empty() {
        return own.to_string();
    }
    let all = auto_title(tab);
    if full {
        return all;
    }
    match all.find(", ") {
        None => all,
        Some(comma) => format!("{}, …", &all[..comma]),
    }
}

// ---- normalisation --------------------------------------------------------

fn fix_sizes(sizes: &[f32], n: usize) -> Vec<f32> {
    let mut out: Vec<f32> = if sizes.len() == n {
        sizes.iter().map(|s| if s.is_finite() && *s > 0.0 { *s } else { 0.0 }).collect()
    } else {
        vec![1.0; n]
    };
    let mut sum: f32 = out.iter().sum();
    if sum <= 0.0 {
        out = vec![1.0; n];
        sum = n as f32;
    }
    out.into_iter().map(|s| s / sum).collect()
}

/// Collapse single-child containers, merge a container into a same-kind parent, fix sizes.
pub fn normalize(node: Option<SplitNode>) -> Option<SplitNode> {
    let node = node?;
    let (axis, id, children, sizes) = match node {
        SplitNode::Leaf { .. } => return Some(node),
        SplitNode::Row { id, children, sizes } => (Axis::Row, id, children, sizes),
        SplitNode::Col { id, children, sizes } => (Axis::Col, id, children, sizes),
    };
    let mut kids: Vec<SplitNode> = Vec::new();
    let mut out_sizes: Vec<f32> = Vec::new();
    let own = fix_sizes(&sizes, children.len());
    for (i, c) in children.into_iter().enumerate() {
        let Some(n) = normalize(Some(c)) else { continue };
        if axis_of(&n) == Some(axis) {
            let (gk, gs) = match n {
                SplitNode::Row { children, sizes, .. } | SplitNode::Col { children, sizes, .. } => (children, sizes),
                SplitNode::Leaf { .. } => unreachable!(),
            };
            for (j, g) in gk.into_iter().enumerate() {
                kids.push(g);
                out_sizes.push(own[i] * gs.get(j).copied().unwrap_or(0.0));
            }
        } else {
            kids.push(n);
            out_sizes.push(own[i]);
        }
    }
    match kids.len() {
        0 => None,
        1 => kids.pop(),
        n => Some(container(axis, id, kids, fix_sizes(&out_sizes, n))),
    }
}

// ---- edits ------------------------------------------------------------------

pub fn map_leaf(node: Option<&SplitNode>, id: &str, f: &dyn Fn(Leaf) -> Leaf) -> Option<SplitNode> {
    let node = node?;
    Some(match node {
        SplitNode::Leaf { id: lid, sources, mod_mode } => {
            if lid == id {
                f(Leaf { id: lid.clone(), sources: sources.clone(), mod_mode: *mod_mode }).into()
            } else {
                node.clone()
            }
        }
        SplitNode::Row { id: cid, children, sizes } => {
            SplitNode::Row { id: cid.clone(), children: children.iter().filter_map(|c| map_leaf(Some(c), id, f)).collect(), sizes: sizes.clone() }
        }
        SplitNode::Col { id: cid, children, sizes } => {
            SplitNode::Col { id: cid.clone(), children: children.iter().filter_map(|c| map_leaf(Some(c), id, f)).collect(), sizes: sizes.clone() }
        }
    })
}

pub fn remove_leaf(node: Option<&SplitNode>, id: &str) -> Option<SplitNode> {
    let node = node?;
    let (axis, cid, children, sizes) = match node {
        SplitNode::Leaf { id: lid, .. } => return if lid == id { None } else { Some(node.clone()) },
        SplitNode::Row { id, children, sizes } => (Axis::Row, id, children, sizes),
        SplitNode::Col { id, children, sizes } => (Axis::Col, id, children, sizes),
    };
    let mut kids = Vec::new();
    let mut out_sizes = Vec::new();
    for (i, c) in children.iter().enumerate() {
        if let Some(r) = remove_leaf(Some(c), id) {
            kids.push(r);
            out_sizes.push(sizes.get(i).copied().unwrap_or(1.0 / children.len() as f32));
        }
    }
    if kids.is_empty() {
        return None;
    }
    normalize(Some(container(axis, cid.clone(), kids, out_sizes)))
}

/// Insert `add` next to the node with id `target_id`, on the given edge, taking half of its room.
pub fn insert_at(root: Option<&SplitNode>, target_id: &str, add: SplitNode, edge: Edge) -> Option<SplitNode> {
    let Some(root) = root else { return Some(add) };
    let axis = if matches!(edge, Edge::Left | Edge::Right) { Axis::Row } else { Axis::Col };
    let before = matches!(edge, Edge::Left | Edge::Top);

    let wrap = |target: SplitNode| -> SplitNode {
        container(axis, uid(), if before { vec![add.clone(), target] } else { vec![target, add.clone()] }, vec![0.5, 0.5])
    };

    fn walk(node: &SplitNode, target_id: &str, axis: Axis, before: bool, add: &SplitNode, wrap: &dyn Fn(SplitNode) -> SplitNode) -> SplitNode {
        match node {
            SplitNode::Leaf { id, .. } => {
                if id == target_id {
                    wrap(node.clone())
                } else {
                    node.clone()
                }
            }
            SplitNode::Row { id, children, sizes } | SplitNode::Col { id, children, sizes } => {
                let this_axis = axis_of(node).unwrap();
                let idx = children.iter().position(|c| node_id(c) == target_id);
                if let Some(idx) = idx {
                    if this_axis == axis {
                        let mut children = children.clone();
                        let mut sizes = sizes.clone();
                        let half = sizes[idx] / 2.0;
                        sizes[idx] = half;
                        let at = if before { idx } else { idx + 1 };
                        children.insert(at, add.clone());
                        sizes.insert(at, half);
                        return container(this_axis, id.clone(), children, sizes);
                    }
                    let kids = children.iter().enumerate().map(|(i, c)| if i == idx { wrap(c.clone()) } else { c.clone() }).collect();
                    return container(this_axis, id.clone(), kids, sizes.clone());
                }
                let kids = children.iter().map(|c| walk(c, target_id, axis, before, add, wrap)).collect();
                container(this_axis, id.clone(), kids, sizes.clone())
            }
        }
    }
    normalize(Some(walk(root, target_id, axis, before, &add, &wrap)))
}

/// Move leaf `src_id` onto the `edge` of leaf `target_id`.
pub fn move_leaf(root: Option<&SplitNode>, src_id: &str, target_id: &str, edge: Edge) -> Option<SplitNode> {
    if src_id == target_id {
        return root.cloned();
    }
    let Some(src) = find_leaf(root, src_id) else { return root.cloned() };
    if find_leaf(root, target_id).is_none() {
        return root.cloned();
    }
    let without = remove_leaf(root, src_id);
    insert_at(without.as_ref(), target_id, src.into(), edge)
}

pub fn set_sizes(node: Option<&SplitNode>, container_id: &str, new_sizes: &[f32]) -> Option<SplitNode> {
    let node = node?;
    Some(match node {
        SplitNode::Leaf { .. } => node.clone(),
        SplitNode::Row { id, children, sizes } | SplitNode::Col { id, children, sizes } => {
            let axis = axis_of(node).unwrap();
            if id == container_id {
                container(axis, id.clone(), children.clone(), new_sizes.to_vec())
            } else {
                let _ = sizes;
                container(axis, id.clone(), children.iter().filter_map(|c| set_sizes(Some(c), container_id, new_sizes)).collect(), sizes.clone())
            }
        }
    })
}

// ---- layout -------------------------------------------------------------------

pub fn default_layout() -> Layout {
    let tab = new_tab(Some(vec![SourceRef { platform: Platform::Twitch, channel: "a_g_u__".into() }]));
    Layout { active: tab.id.clone(), tabs: vec![tab] }
}

fn clean_node(n: &Value) -> Option<SplitNode> {
    let o = n.as_object()?;
    let id_of = |o: &serde_json::Map<String, Value>| match o.get("id").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => uid(),
    };
    match o.get("kind").and_then(|k| k.as_str()) {
        Some("leaf") => {
            let sources: Vec<SourceRef> = o
                .get("sources")
                .and_then(|s| s.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| {
                            let platform = serde_json::from_value::<Platform>(s.get("platform")?.clone()).ok()?;
                            let channel = s.get("channel")?.as_str()?;
                            if channel.is_empty() {
                                return None;
                            }
                            Some(SourceRef { platform, channel: channel.to_string() })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if sources.is_empty() {
                return None;
            }
            let mod_mode = o.get("modMode").map(|v| !matches!(v, Value::Null | Value::Bool(false)) && v.as_f64() != Some(0.0) && v.as_str() != Some("")).unwrap_or(false);
            Some(SplitNode::Leaf { id: id_of(o), sources, mod_mode })
        }
        Some(kind @ ("row" | "col")) => {
            let children: Vec<SplitNode> = o.get("children").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(clean_node).collect()).unwrap_or_default();
            if children.is_empty() {
                return None;
            }
            let mut sizes: Vec<f32> = o.get("sizes").and_then(|s| s.as_array()).map(|a| a.iter().map(|v| v.as_f64().unwrap_or(f64::NAN) as f32).collect()).unwrap_or_default();
            sizes.truncate(children.len());
            Some(container(if kind == "row" { Axis::Row } else { Axis::Col }, id_of(o), children, sizes))
        }
        _ => None,
    }
}

/// Make whatever was on disk safe to render. Returns the default layout when nothing usable is there.
pub fn sanitize_layout(raw: &Value) -> Layout {
    let Some(tabs_raw) = raw.get("tabs").and_then(|t| t.as_array()) else { return default_layout() };
    let tabs: Vec<Tab> = tabs_raw
        .iter()
        .filter(|t| t.is_object())
        .map(|t| Tab {
            id: match t.get("id").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => uid(),
            },
            title: t.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            root: normalize(t.get("root").and_then(clean_node)),
        })
        .collect();
    if tabs.is_empty() {
        return default_layout();
    }
    let want = raw.get("active").and_then(|v| v.as_str());
    let active = if tabs.iter().any(|t| Some(t.id.as_str()) == want) { want.unwrap().to_string() } else { tabs[0].id.clone() };
    Layout { tabs, active }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(c: &str) -> Vec<SourceRef> {
        vec![SourceRef { platform: Platform::Twitch, channel: c.into() }]
    }
    fn leaf(id: &str) -> SplitNode {
        SplitNode::Leaf { id: id.into(), sources: src(id), mod_mode: false }
    }

    #[test]
    fn insert_splits_the_target_and_remove_collapses_it_again() {
        let root = leaf("a");
        let r = insert_at(Some(&root), "a", leaf("b"), Edge::Right).unwrap();
        assert!(matches!(&r, SplitNode::Row { sizes, .. } if sizes == &vec![0.5, 0.5]));
        let r2 = insert_at(Some(&r), "b", leaf("c"), Edge::Bottom).unwrap();
        assert_eq!(leaves(Some(&r2)).len(), 3);
        let back = remove_leaf(Some(&r2), "c").unwrap();
        assert_eq!(back, r);
        assert_eq!(remove_leaf(Some(&leaf("a")), "a"), None);
    }

    #[test]
    fn same_axis_insert_halves_the_neighbour_and_normalize_merges_nested_rows() {
        let r = insert_at(Some(&leaf("a")), "a", leaf("b"), Edge::Right).unwrap();
        let r = insert_at(Some(&r), "a", leaf("c"), Edge::Right).unwrap();
        match &r {
            SplitNode::Row { children, sizes, .. } => {
                assert_eq!(children.iter().map(|c| node_id(c).to_string()).collect::<Vec<_>>(), vec!["a", "c", "b"]);
                assert_eq!(sizes, &vec![0.25, 0.25, 0.5]);
            }
            _ => panic!("expected a row"),
        }
    }

    #[test]
    fn move_leaf_puts_the_source_on_the_target_edge() {
        let r = insert_at(Some(&leaf("a")), "a", leaf("b"), Edge::Right).unwrap();
        let m = move_leaf(Some(&r), "b", "a", Edge::Left).unwrap();
        assert_eq!(leaves(Some(&m)).iter().map(|l| l.id.clone()).collect::<Vec<_>>(), vec!["b", "a"]);
    }

    #[test]
    fn titles() {
        let tab = Tab { id: "t".into(), title: String::new(), root: insert_at(Some(&leaf("EsfandTV")), "EsfandTV", SplitNode::Leaf { id: "x".into(), sources: src("@esfandtv"), mod_mode: false }, Edge::Right) };
        assert_eq!(auto_title(&tab), "EsfandTV");
        assert_eq!(tab_title(&Tab { id: "t".into(), title: String::new(), root: None }, false), "New tab");
        let s = SourceRef { platform: Platform::Youtube, channel: "https://www.youtube.com/watch?v=abc123".into() };
        assert_eq!(short_name(&s), "abc123");
    }

    #[test]
    fn a_whisper_conversation_is_named_after_the_other_user() {
        let w = SourceRef { platform: Platform::Twitch, channel: "whisper:bob".into() };
        assert!(is_whisper(&w));
        assert!(is_whisper_key(&source_key(&w)));
        assert!(!is_whisper_key("twitch:bob") && !is_whisper_key("kick:whisper:bob"));
        assert!(!is_whisper(&SourceRef { platform: Platform::Kick, channel: "whisper:bob".into() }));
        assert!(!is_whisper(&SourceRef { platform: Platform::Twitch, channel: "bob".into() }));
        assert_eq!(short_name(&w), "bob");
        let only = new_tab(Some(vec![w.clone()]));
        assert_eq!(tab_title(&only, false), "bob");
        assert!(whisper_only(&only));
        assert!(!whisper_only(&new_tab(Some(vec![w, SourceRef { platform: Platform::Twitch, channel: "bob".into() }]))));
        assert!(!whisper_only(&new_tab(None)));
    }

    #[test]
    fn sanitize_drops_junk_and_falls_back_to_the_default() {
        let l = sanitize_layout(&serde_json::json!({"tabs": [{"id": "t1", "title": 5, "root": {"kind": "leaf", "id": "l1", "sources": [{"platform": "twitch", "channel": "x"}, {"platform": "nope", "channel": "y"}]}}], "active": "zzz"}));
        assert_eq!(l.active, "t1");
        assert_eq!(leaves(l.tabs[0].root.as_ref())[0].sources.len(), 1);
        assert_eq!(sanitize_layout(&serde_json::json!(null)).tabs.len(), 1);
    }
}
