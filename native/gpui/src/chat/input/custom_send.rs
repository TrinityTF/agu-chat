//! Sends what a custom command expanded to: each line goes the way a typed line would (a "/..." line runs as that
//! slash command, anything else is a message to the targets), one after another, stopping at the first failure.
use std::sync::Arc;

use agu_chat_core::protocol::{AguBridge, SourceRef};
use agu_chat_model::input::custom_commands::{line_action, LineAction};

use super::send_box::{post_text, send_path};
use super::slash::run_slash_command;
use crate::layout::tree::is_whisper;

/// The longest message a chat takes.
const LINE_MAX: usize = 500;

/// What happened: toasts to show (text, warning) and whether every line went through.
pub struct LinesOutcome {
    pub toasts: Vec<(String, bool)>,
    pub ok: bool,
}

/// `targets`: each chat with the message id to reply to there, if any (used by the first message line only).
pub async fn run_lines(bridge: &Arc<dyn AguBridge>, targets: &[(SourceRef, Option<String>)], lines: &[String], last: Option<String>, pinned: Option<String>) -> LinesOutcome {
    let mut out = LinesOutcome { toasts: Vec::new(), ok: true };
    let slash_ok = targets.len() == 1 && targets[0].0.platform == agu_chat_core::protocol::Platform::Twitch && !is_whisper(&targets[0].0);
    let mut replied = false;
    for raw in lines {
        let line: String = raw.trim().chars().take(LINE_MAX).collect();
        if line.is_empty() {
            continue;
        }
        let fail = |out: &mut LinesOutcome, t: String| {
            out.toasts.push((t, true));
            out.ok = false;
        };
        match line_action(&line, slash_ok, targets.len() > 1) {
            LineAction::Confirm(cmd) => {
                fail(&mut out, format!("Type /{cmd} yourself to confirm it"));
                return out;
            }
            LineAction::NeedsSingleTwitch => {
                fail(&mut out, "Commands run on a single Twitch chat: pick it in the target menu".into());
                return out;
            }
            LineAction::Slash => match run_slash_command(bridge, &targets[0].0, &line, last.clone(), pinned.clone()).await {
                Ok((ok, toast)) => {
                    if let Some((t, warn)) = toast {
                        out.toasts.push((t, warn));
                    }
                    if !ok {
                        out.ok = false;
                        return out;
                    }
                }
                Err(e) => {
                    fail(&mut out, e);
                    return out;
                }
            },
            LineAction::Text => {
                let multi = targets.len() > 1;
                let first = !replied;
                replied = true;
                let sends = targets.iter().filter_map(|(s, reply)| {
                    let path = send_path(s.platform)?;
                    let (bridge, line) = (bridge, &line);
                    let reply = if first { reply.clone() } else { None };
                    Some(async move { (s.platform, post_text(bridge, s, path, line, reply.as_deref()).await) })
                });
                let outcomes = futures::future::join_all(sends).await;
                let failures = super::send_box::send_failures(outcomes, multi);
                if !failures.is_empty() {
                    fail(&mut out, failures.join(" - "));
                    return out;
                }
            }
        }
    }
    out
}
