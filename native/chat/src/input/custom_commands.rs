//! Custom commands: a short name the user types (`/copypasta`, `/savaget`) that the send box replaces with a
//! saved text before sending. The same idea and the same placeholders as Chatterino, so commands imported
//! from Chatterino keep working unchanged.
//!
//! Placeholders in the text:
//! - `{1}`, `{2}` … one word typed after the name; `{1+}`, `{2+}` … that word and everything after it.
//! - `{channel.name}` (also `{channel}`), `{my.name}`, `{user.name}` (also `{user}`), `{msg.id}`, `{msg.text}`:
//!   the chat, your own name, and, when run from a message's menu, that message's sender, id and text.
//! - `{name;fallback}` uses `fallback` when `name` is empty. `{{` is a literal `{`.
//! - Anything else in braces is left as typed.
//!
//! A text with several lines sends one message per line.
use super::autocomplete::{command_query, AutoMatch, AutoMode, AutoState};
use agu_chat_core::protocol::CustomCommand;
use serde_json::Value;

/// What a command can fill in besides the typed words. Empty strings when unknown.
#[derive(Clone, Debug, Default)]
pub struct CommandContext {
    pub channel: String,
    pub me: String,
    pub user: String,
    pub msg_id: String,
    pub msg_text: String,
}

/// The command `name` stands for, compared without case. Names are stored as typed, so `/d1` matches only
/// `/d1` and a name without a slash matches the bare word, as in Chatterino.
pub fn find<'a>(cmds: &'a [CustomCommand], word: &str) -> Option<&'a CustomCommand> {
    if word.is_empty() {
        return None;
    }
    cmds.iter().find(|c| !c.name.is_empty() && c.name.eq_ignore_ascii_case(word))
}

/// The messages to send for `input`, or None when its first word is not a custom command (send it as typed).
/// Empty lines are dropped; an expansion with no text at all gives an empty list.
pub fn expand_input(input: &str, cmds: &[CustomCommand], ctx: &CommandContext) -> Option<Vec<String>> {
    let trimmed = input.trim_start();
    let first = trimmed.split_whitespace().next()?;
    let cmd = find(cmds, first)?;
    let words: Vec<&str> = trimmed.split_whitespace().skip(1).collect();
    Some(lines(&expand(&cmd.text, &words, ctx)))
}

/// The messages to send when `cmd` is run from a message's menu: no typed words, the message fills the rest.
pub fn expand_for_message(cmd: &CustomCommand, ctx: &CommandContext) -> Vec<String> {
    lines(&expand(&cmd.text, &[], ctx))
}

fn lines(s: &str) -> Vec<String> {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

/// Fills every placeholder in `text`.
pub fn expand(text: &str, words: &[&str], ctx: &CommandContext) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(stripped) = after.strip_prefix('{') {
            out.push('{');
            rest = stripped;
            continue;
        }
        match after.find('}') {
            Some(j) if !after[..j].contains('{') => {
                let inner = &after[..j];
                match fill(inner, words, ctx) {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('{');
                        out.push_str(inner);
                        out.push('}');
                    }
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of one placeholder, or None to leave it as typed.
fn fill(inner: &str, words: &[&str], ctx: &CommandContext) -> Option<String> {
    let (key, fallback) = match inner.split_once(';') {
        Some((k, f)) => (k.trim(), Some(f)),
        None => (inner.trim(), None),
    };
    let value = if let Some(n) = key.strip_suffix('+').and_then(|n| n.parse::<usize>().ok()) {
        if n == 0 {
            return None;
        }
        words.get(n - 1..).map(|w| w.join(" ")).unwrap_or_default()
    } else if let Ok(n) = key.parse::<usize>() {
        if n == 0 {
            return None;
        }
        words.get(n - 1).map(|w| w.to_string()).unwrap_or_default()
    } else {
        match key.to_ascii_lowercase().as_str() {
            "channel.name" | "channel" => ctx.channel.clone(),
            "my.name" => ctx.me.clone(),
            "user.name" | "user" => ctx.user.clone(),
            "msg.id" => ctx.msg_id.clone(),
            "msg.text" => ctx.msg_text.clone(),
            _ => return None,
        }
    };
    if value.is_empty() {
        if let Some(f) = fallback {
            return Some(f.to_string());
        }
    }
    Some(value)
}

/// Commands from a Chatterino `commands.json` (`{"commands": [{name, func, showInMsgContextMenu}]}`), or a
/// bare list of the same entries. Entries without a name or text are skipped.
pub fn parse_chatterino(json: &str) -> Result<Vec<CustomCommand>, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "That is not a Chatterino commands file".to_string())?;
    let list = v.get("commands").and_then(Value::as_array).or_else(|| v.as_array()).ok_or("That is not a Chatterino commands file")?;
    Ok(list
        .iter()
        .filter_map(|e| {
            let name = e.get("name")?.as_str()?.trim();
            let text = e.get("func")?.as_str()?;
            if name.is_empty() || name.contains(char::is_whitespace) || text.trim().is_empty() {
                return None;
            }
            Some(CustomCommand {
                name: name.to_string(),
                text: text.to_string(),
                in_menu: e.get("showInMsgContextMenu").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect())
}

/// `existing` with `incoming` added. A command whose name is already there (ignoring case) is skipped, so
/// importing twice adds nothing. Returns the list and how many were added.
pub fn merge(existing: &[CustomCommand], incoming: Vec<CustomCommand>) -> (Vec<CustomCommand>, usize) {
    let mut out = existing.to_vec();
    let mut added = 0;
    for c in incoming {
        if find(&out, &c.name).is_none() {
            out.push(c);
            added += 1;
        }
    }
    (out, added)
}

/// Where Chatterino keeps its commands on this computer, when it is installed. None on phones.
pub fn chatterino_commands_file() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let candidates: Vec<PathBuf> = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(|a| vec![PathBuf::from(a).join("Chatterino2").join("Settings")]).unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| {
                let base = PathBuf::from(h).join("Library").join("Application Support");
                vec![base.join("chatterino").join("Settings"), base.join("Chatterino2").join("Settings")]
            })
            .unwrap_or_default()
    } else if cfg!(target_os = "linux") {
        std::env::var_os("HOME")
            .map(|h| {
                let base = PathBuf::from(h).join(".local").join("share");
                vec![base.join("chatterino").join("Settings"), base.join("Chatterino2").join("Settings")]
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    candidates.into_iter().map(|d| d.join("commands.json")).find(|p| p.is_file())
}

/// A name as the settings save it: trimmed, with a "/" in front when it has none.
pub fn clean_name(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t.starts_with('/') {
        t.to_string()
    } else {
        format!("/{t}")
    }
}

/// Why a command can't be saved, in words for the settings page. `others`: the names of the other commands.
pub fn command_error<'a>(name: &str, text: &str, others: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let n = name.trim().trim_start_matches('/');
    if n.is_empty() {
        return Some("Give the command a name");
    }
    if n.contains(char::is_whitespace) {
        return Some("The name can't have spaces");
    }
    if n.chars().count() > 39 {
        return Some("The name is too long");
    }
    if text.trim().is_empty() {
        return Some("Give the command some text");
    }
    let clean = clean_name(name);
    if others.into_iter().any(|o| o.eq_ignore_ascii_case(&clean)) {
        return Some("There is already a command with that name");
    }
    None
}

/// How the send box treats one line of an expansion, the same as a typed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineAction {
    /// a slash command that runs on the one Twitch chat
    Slash,
    /// sent as a message to the current targets
    Text,
    /// a slash command with several chats selected
    NeedsSingleTwitch,
    /// `/clear` and `/raid` ask for a confirmation, so they are not run from a command
    Confirm(String),
}

/// `slash_ok`: exactly one Twitch chat (not a whisper) is the target. `several`: more than one chat is.
pub fn line_action(line: &str, slash_ok: bool, several: bool) -> LineAction {
    let t = line.trim();
    if !t.starts_with('/') || super::slash::is_me(t) {
        return LineAction::Text;
    }
    if slash_ok {
        let (cmd, _) = super::slash::parse_command(t).unwrap_or_default();
        return if cmd == "clear" || cmd == "raid" { LineAction::Confirm(cmd) } else { LineAction::Slash };
    }
    if several {
        LineAction::NeedsSingleTwitch
    } else {
        LineAction::Text
    }
}

/// The commands that start with `query` (typed after the "/"), as autocomplete rows: the name, and the first
/// line of the text as the hint. Only names that start with "/" can be completed.
pub fn auto_matches(query: &str, cmds: &[CustomCommand]) -> Vec<AutoMatch> {
    let q = query.to_lowercase();
    cmds.iter()
        .filter_map(|c| {
            let name = c.name.strip_prefix('/')?;
            if name.is_empty() || !name.to_lowercase().starts_with(&q) {
                return None;
            }
            let first = c.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
            let mut label: String = first.chars().take(40).collect();
            if first.chars().count() > 40 {
                label.push_str("...");
            }
            Some(AutoMatch { name: name.to_string(), label, ..Default::default() })
        })
        .collect()
}

/// `auto` (what the built-in commands offered for the text before the caret) with the custom commands added.
/// A custom command hides a built-in of the same name, as it wins when sent.
pub fn with_custom_auto(auto: Option<AutoState>, val: &str, pos: usize, cmds: &[CustomCommand]) -> Option<AutoState> {
    if cmds.is_empty() {
        return auto;
    }
    let before = &val[..pos.min(val.len())];
    let Some(query) = command_query(before) else { return auto };
    let custom = auto_matches(query, cmds);
    if custom.is_empty() {
        return auto;
    }
    let mut state = match auto {
        Some(a) if a.mode == AutoMode::Cmd => a,
        _ => AutoState { mode: AutoMode::Cmd, start: 0, end: before.len(), matches: Vec::new(), idx: 0 },
    };
    state.matches.retain(|m| !custom.iter().any(|c| c.name.eq_ignore_ascii_case(&m.name)));
    state.matches.extend(custom);
    Some(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str, text: &str) -> CustomCommand {
        CustomCommand { name: name.into(), text: text.into(), in_menu: false }
    }

    fn ctx() -> CommandContext {
        CommandContext { channel: "a_g_u__".into(), me: "agu".into(), user: "savaget".into(), msg_id: "m1".into(), msg_text: "hi there".into() }
    }

    #[test]
    fn plain_text_is_sent_as_saved() {
        let cmds = vec![cmd("/copypasta", "long pasta text")];
        assert_eq!(expand_input("/copypasta", &cmds, &ctx()), Some(vec!["long pasta text".to_string()]));
    }

    #[test]
    fn not_a_command_is_left_alone() {
        let cmds = vec![cmd("/p", "x")];
        assert_eq!(expand_input("/pp hello", &cmds, &ctx()), None);
        assert_eq!(expand_input("hello", &cmds, &ctx()), None);
        assert_eq!(expand_input("", &cmds, &ctx()), None);
    }

    #[test]
    fn names_match_without_case_and_with_or_without_slash_as_saved() {
        let cmds = vec![cmd("/Savaget", "@savagetarzan hi"), cmd("gm", "good morning")];
        assert!(expand_input("/savaget", &cmds, &ctx()).is_some());
        assert!(expand_input("gm", &cmds, &ctx()).is_some());
        assert!(expand_input("/gm", &cmds, &ctx()).is_none());
    }

    #[test]
    fn word_placeholders() {
        let cmds = vec![cmd("/hi", "hello {1}, {2+}!"), cmd("/to", "/timeout {1} {2;600}")];
        assert_eq!(expand_input("/hi bob how are you", &cmds, &ctx()).unwrap(), vec!["hello bob, how are you!"]);
        assert_eq!(expand_input("/hi", &cmds, &ctx()).unwrap(), vec!["hello , !"]);
        assert_eq!(expand_input("/to bob", &cmds, &ctx()).unwrap(), vec!["/timeout bob 600"]);
        assert_eq!(expand_input("/to bob 60", &cmds, &ctx()).unwrap(), vec!["/timeout bob 60"]);
    }

    #[test]
    fn context_placeholders_and_escapes() {
        let c = ctx();
        assert_eq!(expand("{user.name} in {channel.name} by {my.name}", &[], &c), "savaget in a_g_u__ by agu");
        assert_eq!(expand("/delete {msg.id}", &[], &c), "/delete m1");
        assert_eq!(expand("{{1} {unknown} {0} {", &["x"], &c), "{1} {unknown} {0} {");
    }

    #[test]
    fn several_lines_send_several_messages() {
        let cmds = vec![cmd("/two", "first\n\n second ")];
        assert_eq!(expand_input("/two", &cmds, &ctx()).unwrap(), vec!["first", "second"]);
    }

    #[test]
    fn chatterino_file_imports() {
        let json = r#"{"commands":[
            {"name":"/d1","func":"/me Brothers gather","showInMsgContextMenu":true},
            {"name":"/delete-2","func":"/delete {msg.id}","showInMsgContextMenu":false},
            {"name":"","func":"x"},
            {"name":"/empty","func":"  "}
        ]}"#;
        let got = parse_chatterino(json).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], CustomCommand { name: "/d1".into(), text: "/me Brothers gather".into(), in_menu: true });
        assert!(parse_chatterino("not json").is_err());
        assert!(parse_chatterino("{}").is_err());
    }

    #[test]
    fn merging_skips_names_already_there() {
        let existing = vec![cmd("/d1", "mine")];
        let (out, added) = merge(&existing, vec![cmd("/D1", "theirs"), cmd("/d2", "new")]);
        assert_eq!(added, 1);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "mine");
    }

    #[test]
    fn lines_run_like_typed_ones() {
        assert_eq!(line_action("hello", false, false), LineAction::Text);
        assert_eq!(line_action("/me waves", true, false), LineAction::Text);
        assert_eq!(line_action("/delete abc", true, false), LineAction::Slash);
        assert_eq!(line_action("/delete abc", false, true), LineAction::NeedsSingleTwitch);
        assert_eq!(line_action("/delete abc", false, false), LineAction::Text);
        assert_eq!(line_action("/clear", true, false), LineAction::Confirm("clear".into()));
    }

    #[test]
    fn autocomplete_offers_slash_names_with_a_preview() {
        let cmds = vec![cmd("/copypasta", "first line that is quite long indeed, more than forty characters
second"), cmd("cop", "bare"), cmd("/other", "x")];
        let m = auto_matches("co", &cmds);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "copypasta");
        assert!(m[0].label.starts_with("first line") && m[0].label.ends_with("..."));
    }

    #[test]
    fn custom_matches_join_the_builtin_list() {
        let cmds = vec![cmd("/timeout", "mine"), cmd("/copypasta", "x")];
        let builtin = AutoState { mode: AutoMode::Cmd, start: 0, end: 2, matches: vec![AutoMatch { name: "timeout".into(), ..Default::default() }], idx: 0 };
        let a = with_custom_auto(Some(builtin), "/t", 2, &cmds).unwrap();
        assert_eq!(a.matches.len(), 1);
        assert_eq!(a.matches[0].label, "mine");
        let a = with_custom_auto(None, "/co", 3, &cmds).unwrap();
        assert_eq!((a.mode, a.end, a.matches.len()), (AutoMode::Cmd, 3, 1));
        assert!(with_custom_auto(None, "hello", 5, &cmds).is_none());
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(clean_name(" hi "), "/hi");
        assert_eq!(clean_name("/hi"), "/hi");
        assert_eq!(command_error("", "x", []), Some("Give the command a name"));
        assert_eq!(command_error("/a b", "x", []), Some("The name can't have spaces"));
        assert_eq!(command_error("/a", "  ", []), Some("Give the command some text"));
        assert_eq!(command_error("A", "x", ["/a"]), Some("There is already a command with that name"));
        assert_eq!(command_error("/a", "x", ["/b"]), None);
    }
}
