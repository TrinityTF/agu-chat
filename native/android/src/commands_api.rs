//! Settings > Commands: what the screen needs besides `settings.get` / `settings.set` (the list itself is the
//! `customCommands` setting): the check of a command before it is saved, and the import of a Chatterino-format
//! `commands.json` the document picker read. The rules are `agu_chat_model::input::custom_commands`, the desktop's.
use agu_chat_core::protocol::CustomCommand;
use agu_chat_model::input::custom_commands::{clean_name, command_error, merge, parse_chatterino, COMMANDS_HELP};
use serde_json::{json, Value};

/// `commands.check`: `{name, text, index?}` (`index`: the command being edited, left out of the duplicate check).
/// Answers `{error, name}`: the reason it can't be saved (null when it can) and the name as it is saved.
pub fn check(cmds: &[CustomCommand], args: &Value) -> Value {
    let name = args["name"].as_str().unwrap_or("");
    let text = args["text"].as_str().unwrap_or("");
    let skip = args["index"].as_u64().map(|i| i as usize);
    let others = cmds.iter().enumerate().filter(|(i, _)| Some(*i) != skip).map(|(_, c)| c.name.as_str());
    json!({ "error": command_error(name, text, others), "name": clean_name(name) })
}

/// The commands of a Chatterino-format file's text merged into `cmds`. `Err`: the line to show (not a commands file).
/// `Ok((list, status))`: `list` is the new list when something was added (None when nothing was), `status` the line.
pub fn import(cmds: &[CustomCommand], text: &str) -> Result<(Option<Vec<CustomCommand>>, String), String> {
    let incoming = parse_chatterino(text)?;
    let (merged, added) = merge(cmds, incoming);
    if added == 0 {
        return Ok((None, "No new commands to import".into()));
    }
    Ok((Some(merged), format!("Imported {added} command{}", if added == 1 { "" } else { "s" })))
}

/// `commands.import`'s answer: `{status, warn, commands?}`.
pub fn import_json(cmds: &[CustomCommand], text: &str) -> Value {
    match import(cmds, text) {
        Err(e) => json!({"status": e, "warn": true}),
        Ok((Some(list), status)) => json!({"status": status, "warn": false, "commands": list}),
        Ok((None, status)) => json!({"status": status, "warn": false}),
    }
}

pub fn help() -> &'static str {
    COMMANDS_HELP
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(n: &str, t: &str) -> CustomCommand {
        CustomCommand { name: n.into(), text: t.into(), in_menu: false }
    }

    #[test]
    fn a_command_is_checked_with_the_desktops_words() {
        let list = vec![cmd("/a", "x"), cmd("/b", "y")];
        assert_eq!(check(&list, &json!({"name": "c", "text": "z"})), json!({"error": null, "name": "/c"}));
        assert_eq!(check(&list, &json!({"name": " ", "text": "z"}))["error"], "Give the command a name");
        assert_eq!(check(&list, &json!({"name": "a b", "text": "z"}))["error"], "The name can't have spaces");
        assert_eq!(check(&list, &json!({"name": "c", "text": " "}))["error"], "Give the command some text");
        assert_eq!(check(&list, &json!({"name": "/A", "text": "z"}))["error"], "There is already a command with that name");
        // editing the command itself is not a duplicate of it
        assert!(check(&list, &json!({"name": "/a", "text": "z", "index": 0}))["error"].is_null());
        assert!(check(&list, &json!({"name": "/b", "text": "z", "index": 0}))["error"].is_string());
    }

    #[test]
    fn importing_adds_the_new_ones_and_says_how_many() {
        let file = r#"{"commands":[{"name":"/hi","func":"hello","showInMsgContextMenu":true},{"name":"/a","func":"dup"},{"name":"/two words","func":"x"}]}"#;
        let list = vec![cmd("/a", "x")];
        let j = import_json(&list, file);
        assert_eq!((j["status"].as_str(), j["warn"].as_bool()), (Some("Imported 1 command"), Some(false)));
        assert_eq!(j["commands"].as_array().unwrap().len(), 2);
        assert_eq!(j["commands"][1], json!({"name": "/hi", "text": "hello", "inMenu": true}));
        // again: nothing new
        let again = vec![cmd("/a", "x"), cmd("/hi", "hello")];
        assert_eq!(import_json(&again, file), json!({"status": "No new commands to import", "warn": false}));
        // not a commands file
        assert_eq!(import_json(&list, "nope"), json!({"status": "That is not a Chatterino commands file", "warn": true}));
        assert_eq!(import_json(&list, "{\"a\":1}")["warn"], true);
        // a bare list works too, and several are counted
        let bare = r#"[{"name":"/x","func":"1"},{"name":"/y","func":"2"}]"#;
        assert_eq!(import_json(&[], bare)["status"], "Imported 2 commands");
    }
}
