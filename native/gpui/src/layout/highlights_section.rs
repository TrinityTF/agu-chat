//! Port of src/renderer/layout/HighlightsSection.tsx: Settings > Highlights: the own-name mention alerts, the
//! alert sound, and the user's highlight rules (add, edit, reorder, delete), plus a small tester to check a rule
//! against a sample message. The file dialog of `agu.sounds.pick()` is GPUI's `prompt_for_paths` followed by
//! `sounds_import` (the copy into `<data dir>/sounds`).
use std::sync::Arc;

use agu_chat_core::protocol::HighlightRule;
use gpui::{div, prelude::*, px, AnyElement, ClickEvent, Context, PathPromptOptions, Window};
use serde_json::json;

use super::settings_dialog::SettingsDialog;
use super::settings_widgets::{checkbox, eid, txt};
use super::tree::uid;
use super::ui::{btn, toggle, BtnOpts};
use crate::chat::highlights::{compile_highlights, next_rule_color, regex_error, rule_row_style, MatchInput, RULE_COLORS};
use crate::icons;
use crate::text_input::InputStyle;
use crate::theme::{self, a, c};

const KINDS: [(&str, &str); 3] = [("phrase", "Phrase"), ("regex", "Regex"), ("user", "User")];

fn placeholder(kind: &str) -> &'static str {
    match kind {
        "regex" => "Regular expression, e.g. ^!(drop|claim)",
        "user" => "Names, separated by commas",
        _ => "Word or phrase, e.g. giveaway",
    }
}

fn kind_label(kind: &str) -> &'static str {
    KINDS.iter().find(|(k, _)| *k == kind).map(|(_, l)| *l).unwrap_or("Phrase")
}

/// Shown name of a stored sound file: drop the "-<hash>" suffix `Sounds::import` adds.
pub fn sound_label(file: &str) -> String {
    if let Some((stem, ext)) = file.rsplit_once('.') {
        if let Some((base, hash)) = stem.rsplit_once('-') {
            if hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                return format!("{base}.{ext}");
            }
        }
    }
    file.to_string()
}

#[derive(Clone)]
enum SoundTarget {
    Default,
    Rule(String),
}

fn hex_ok(s: &str) -> bool {
    let h = s.trim().trim_start_matches('#');
    h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit())
}

impl SettingsDialog {
    fn rules(&self) -> Vec<HighlightRule> {
        self.settings.highlight_rules.clone()
    }

    fn set_rules(&mut self, rules: Vec<HighlightRule>, cx: &mut Context<Self>) {
        self.set(json!({ "highlightRules": rules }), cx);
    }

    fn update_rule(&mut self, id: &str, f: impl FnOnce(&mut HighlightRule), cx: &mut Context<Self>) {
        let mut rules = self.rules();
        if let Some(r) = rules.iter_mut().find(|r| r.id == id) {
            f(r);
            self.set_rules(rules, cx);
        }
    }

    fn move_rule(&mut self, i: usize, dir: i32, cx: &mut Context<Self>) {
        let mut rules = self.rules();
        let j = i as i32 + dir;
        if j < 0 || j as usize >= rules.len() {
            return;
        }
        rules.swap(i, j as usize);
        self.set_rules(rules, cx);
    }

    fn add_rule(&mut self, cx: &mut Context<Self>) {
        let mut rules = self.rules();
        let rule = HighlightRule {
            id: uid(),
            enabled: true,
            kind: "phrase".into(),
            pattern: String::new(),
            case_sensitive: false,
            color: next_rule_color(&rules).to_string(),
            sound: false,
            sound_file: String::new(),
            flash: false,
        };
        self.focus_rule = Some(rule.id.clone());
        rules.push(rule);
        self.set_rules(rules, cx);
    }

    pub(super) fn commit_rule_pattern(&mut self, id: &str, cx: &mut Context<Self>) {
        let text = self.text_of(&format!("hl.{id}.pat"), cx);
        if self.rules().iter().any(|r| r.id == id && r.pattern != text) {
            self.update_rule(id, |r| r.pattern = text, cx);
        }
    }

    pub(super) fn commit_rule_color(&mut self, id: &str, cx: &mut Context<Self>) {
        let text = self.text_of(&format!("color.{id}"), cx);
        if hex_ok(&text) {
            let hex = format!("#{}", text.trim().trim_start_matches('#').to_lowercase());
            self.update_rule(id, |r| r.color = hex, cx);
        }
    }

    pub(super) fn picked_highlights(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        let Some(rest) = id.strip_prefix("hl.") else { return };
        if let Some(rid) = rest.strip_suffix(".kind") {
            let v = value.to_string();
            self.update_rule(rid, |r| r.kind = v, cx);
        } else if let Some(rid) = rest.strip_suffix(".sound") {
            if value == "default" {
                self.update_rule(rid, |r| r.sound_file = String::new(), cx);
            } else {
                self.pick_sound(SoundTarget::Rule(rid.to_string()), cx);
            }
        }
    }

    /// `pickSound`: the OS file dialog, then the copy into the sounds folder.
    fn pick_sound(&mut self, target: SoundTarget, cx: &mut Context<Self>) {
        self.err.remove("sound");
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Choose a sound".into()) });
        let bridge = self.bridge.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let res = bridge.sounds_import(path).await;
            let _ = this.update(cx, |this, cx| match res {
                Ok(file) => match target {
                    SoundTarget::Default => this.set(json!({ "highlightSoundFile": file }), cx),
                    SoundTarget::Rule(id) => this.update_rule(&id, |r| r.sound_file = file, cx),
                },
                Err(e) => {
                    this.err.insert("sound".into(), e);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// `playSound(file, volume)`: a custom file, or the built-in ping when `file` is empty.
    fn play(&self, file: &str, cx: &mut Context<Self>) {
        let svc = cx.global::<crate::chat::services::Services>().clone();
        let bridge = self.bridge.clone();
        let mut load = |f: &str| -> Option<Arc<Vec<u8>>> { futures::executor::block_on(bridge.sounds_read(f.to_string())).ok().map(Arc::new) };
        svc.alerts.play(file, self.settings.highlight_volume as f32, &mut load);
    }

    fn check(&self, id: String, checked: bool, label: &str, title: Option<&str>, cx: &mut Context<Self>, f: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static) -> AnyElement {
        let mut d = div()
            .id(eid(id))
            .flex()
            .items_center()
            .gap(px(4.))
            .text_color(c(theme::MUTED))
            .hover(|s| s.text_color(c(theme::TEXT)))
            .child(checkbox(checked))
            .child(label.to_string())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| f(this, !checked, cx)));
        if let Some(t) = title {
            d = d.tooltip(super::ui::tip(t.to_string()));
        }
        d.into_any_element()
    }

    fn rule_editor(&mut self, r: &HighlightRule, first: bool, last: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let id = r.id.clone();
        let dim = !r.enabled;
        let key = format!("hl.{id}.pat");
        let pat = self.input(&key, placeholder(&r.kind), InputStyle::Field, &r.pattern, true, false, window, cx);
        if self.focus_rule.as_deref() == Some(id.as_str()) {
            self.focus_rule = None;
            pat.update(cx, |t, _| t.focus(window));
        }
        let draft = self.text_of(&key, cx);
        let re_err = if r.kind == "regex" && !draft.trim().is_empty() { regex_error(&draft, r.case_sensitive) } else { None };
        pat.update(cx, |t, cx| t.set_invalid(re_err.is_some(), cx));

        let kind = self.select(&format!("hl.{id}.kind"), &r.kind, &KINDS, true, cx);
        let (i1, i2, i3, i4) = (id.clone(), id.clone(), id.clone(), id.clone());
        let on = r.enabled;
        let tog = toggle(eid(format!("hl-on-{id}")), on, false).cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.update_rule(&i1, |r| r.enabled = !on, cx)));

        let color_open = self.color_open.as_deref() == Some(id.as_str());
        let color_btn = {
            let cid = id.clone();
            let rgb = crate::chat::highlights::parse_hex(&r.color);
            div()
                .id(eid(format!("hl-color-{id}")))
                .h(px(24.))
                .w(px(28.))
                .flex_none()
                .rounded(px(4.))
                .border_1()
                .border_color(c(if color_open { theme::ACCENT } else { theme::LINE }))
                .bg(c(theme::APP))
                .p(px(2.))
                .tooltip(super::ui::tip("Highlight colour"))
                .child(div().size_full().rounded(px(2.)).bg(gpui::rgb(((rgb.0 as u32) << 16) | ((rgb.1 as u32) << 8) | rgb.2 as u32)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.color_open = if this.color_open.as_deref() == Some(cid.as_str()) { None } else { Some(cid.clone()) };
                    cx.notify();
                }))
        };
        let mut color_wrap = div().relative().flex_none().child(color_btn);
        if color_open {
            let hex_key = format!("color.{id}");
            let hex = self.input(&hex_key, "#rrggbb", InputStyle::Small, &r.color, true, false, window, cx);
            let swatches = RULE_COLORS.iter().map(|col| {
                let (rid, col2) = (id.clone(), col.to_string());
                let p = crate::chat::highlights::parse_hex(col);
                div()
                    .id(eid(format!("sw-{id}-{col}")))
                    .size(px(20.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(c(theme::LINE))
                    .bg(gpui::rgb(((p.0 as u32) << 16) | ((p.1 as u32) << 8) | p.2 as u32))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let col3 = col2.clone();
                        this.update_rule(&rid, |r| r.color = col3, cx);
                        this.color_open = None;
                    }))
            });
            color_wrap = color_wrap.child(gpui::deferred(
                div()
                    .absolute()
                    .top_full()
                    .right_0()
                    .mt(px(2.))
                    .w(px(112.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(c(theme::LINE))
                    .bg(c(theme::RAISED))
                    .p(px(6.))
                    .shadow_xl()
                    .occlude()
                    .child(div().flex().flex_wrap().gap(px(4.)).children(swatches))
                    .child(div().mt(px(6.)).child(hex)),
            ).with_priority(3));
        }

        let up = self.icon_btn(format!("hl-up-{id}"), icons::CHEVRON_UP, 14., false, first).tooltip(super::ui::tip("Move up"));
        let up = if first { up } else { up.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_rule_by_id(&i2, -1, cx))) };
        let down = self.icon_btn(format!("hl-dn-{id}"), icons::CHEVRON, 14., false, last).tooltip(super::ui::tip("Move down"));
        let down = if last { down } else { down.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_rule_by_id(&i3, 1, cx))) };
        let del = self.icon_btn(format!("hl-del-{id}"), icons::TRASH, 14., true, false).tooltip(super::ui::tip("Delete rule")).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            let v: Vec<_> = this.rules().into_iter().filter(|x| x.id != i4).collect();
            this.set_rules(v, cx);
        }));

        let (c1, c2, c3) = (id.clone(), id.clone(), id.clone());
        let case = self.check(format!("hl-cs-{id}"), r.case_sensitive, "Case sensitive", Some("Match upper and lower case exactly"), cx, move |this, v, cx| this.update_rule(&c1, |r| r.case_sensitive = v, cx));
        let flash = self.check(format!("hl-fl-{id}"), r.flash, "Flash taskbar", Some("Only while Agu Chat isn't the active window"), cx, move |this, v, cx| this.update_rule(&c2, |r| r.flash = v, cx));
        let sound = self.check(format!("hl-sd-{id}"), r.sound, "Sound", None, cx, move |this, v, cx| this.update_rule(&c3, |r| r.sound = v, cx));
        let sound_ui = r.sound.then(|| {
            let file_label = if r.sound_file.is_empty() { "Choose file…".to_string() } else { sound_label(&r.sound_file) };
            let sel = self.select(&format!("hl.{id}.sound"), if r.sound_file.is_empty() { "default" } else { "file" }, &[("default", "Default sound"), ("file", file_label.as_str())], true, cx);
            let (rid, rid2) = (id.clone(), id.clone());
            let file = if r.sound_file.is_empty() { self.settings.highlight_sound_file.clone() } else { r.sound_file.clone() };
            let change = (!r.sound_file.is_empty()).then(|| {
                div()
                    .id(eid(format!("hl-chg-{id}")))
                    .underline()
                    .hover(|s| s.text_color(c(theme::TEXT)))
                    .child("change")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_sound(SoundTarget::Rule(rid.clone()), cx)))
            });
            let play = self.icon_btn(format!("hl-play-{rid2}"), icons::SPEAKER, 12., false, false).tooltip(super::ui::tip("Play")).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.play(&file, cx)));
            div().flex().items_center().gap(px(4.)).text_color(c(theme::FAINT)).child(sel).children(change).child(play)
        });

        let err_line = if let Some(e) = &re_err {
            Some(format!("Invalid regular expression: {e}"))
        } else {
            self.err.get("sound").cloned()
        };
        let bar = crate::chat::highlights::parse_hex(&r.color);
        div()
            .mb(px(6.))
            .relative()
            .overflow_hidden()
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .pl(px(10.))
            .pr(px(8.))
            .py(px(6.))
            .child(div().absolute().top_0().bottom_0().left_0().w(px(3.)).bg(gpui::rgb(((bar.0 as u32) << 16) | ((bar.1 as u32) << 8) | bar.2 as u32)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(tog)
                    .child(kind)
                    .child(div().min_w_0().flex_1().when(dim, |d| d.opacity(0.5)).child(pat))
                    .child(color_wrap)
                    .child(up)
                    .child(down)
                    .child(del),
            )
            .child(
                div()
                    .mt(px(4.))
                    .pl(px(36.))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(px(12.))
                    .gap_y(px(4.))
                    .text_size(px(11.))
                    .line_height(px(16.5))
                    .when(dim, |d| d.opacity(0.5))
                    .child(case)
                    .child(flash)
                    .child(sound)
                    .children(sound_ui),
            )
            .children(err_line.map(|e| txt(11.).mt(px(4.)).pl(px(36.)).text_color(c(theme::RED_400)).child(e)))
            .into_any_element()
    }

    fn move_rule_by_id(&mut self, id: &str, dir: i32, cx: &mut Context<Self>) {
        if let Some(i) = self.rules().iter().position(|r| r.id == id) {
            self.move_rule(i, dir, cx);
        }
    }

    fn rule_tester(&mut self, rules: &[HighlightRule], window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let user = self.input("hl.test.user", "User", InputStyle::Field, "", false, false, window, cx);
        let text = self.input("hl.test.text", "A sample message", InputStyle::Field, "", false, false, window, cx);
        let (u, t) = (self.text_of("hl.test.user", cx), self.text_of("hl.test.text", cx));
        let hl = compile_highlights(rules);
        let hit = if !t.is_empty() || !u.is_empty() { hl.matches(&MatchInput { text: t.clone(), login: u.clone(), name: u.clone() }) } else { None };
        let result = (!t.is_empty() || !u.is_empty()).then(|| match hit {
            Some(h) => {
                let idx = rules.iter().position(|r| r.id == h.id).unwrap_or(0);
                let st = rule_row_style(&h.color);
                let (br, bg) = (st.bar, st.background);
                div()
                    .mt(px(6.))
                    .rounded(px(2.))
                    .pl(px(8.))
                    .pr(px(8.))
                    .py(px(2.))
                    .bg(a_rgb(bg))
                    .flex()
                    .child(format!("Rule {} ({} ", idx + 1, kind_label(&h.kind).to_lowercase()))
                    .child(div().font_family(theme::MONO).child(h.pattern.clone()))
                    .child(") highlights this.")
            }
            None => div().mt(px(6.)).px(px(8.)).py(px(2.)).text_color(c(theme::FAINT)).child("No rule matches."),
        });
        div()
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .p(px(8.))
            .child(txt(11.).mb(px(6.)).text_color(c(theme::FAINT)).child("TRY IT"))
            .child(div().flex().gap(px(6.)).child(div().w(px(128.)).flex_none().child(user)).child(div().min_w_0().flex_1().child(text)))
            .children(result)
            .into_any_element()
    }

    pub(super) fn render_highlights(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sv = serde_json::to_value(&self.settings).unwrap_or_default();
        let s = self.settings.clone();
        let default_label = if s.highlight_sound_file.is_empty() { "Built-in ping".to_string() } else { sound_label(&s.highlight_sound_file) };
        let choose = btn(eid("hl-choose"), "Choose file…", BtnOpts { small: true, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_sound(SoundTarget::Default, cx)));
        let use_ping = (!s.highlight_sound_file.is_empty()).then(|| btn(eid("hl-ping"), "Use ping", BtnOpts { small: true, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set(json!({ "highlightSoundFile": "" }), cx))));
        let file = s.highlight_sound_file.clone();
        let play = self.icon_btn("hl-play-default".into(), icons::SPEAKER, 14., false, false).tooltip(super::ui::tip("Play")).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.play(&file, cx)));
        let sound_ctl = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().max_w(px(160.)).truncate().text_color(c(theme::MUTED)).child(default_label))
            .child(choose)
            .children(use_ping)
            .child(play);
        let volume = self.slider_row("highlightVolume", s.highlight_volume as f32, 0., 100., 5., "%", cx);
        let sound_err = self.err.get("sound").cloned().filter(|_| true);

        let rules = self.rules();
        let n = rules.len();
        let editors: Vec<AnyElement> = rules.iter().enumerate().map(|(i, r)| self.rule_editor(r, i == 0, i + 1 == n, window, cx)).collect();
        let rules_body: AnyElement = if rules.is_empty() {
            div()
                .mb(px(12.))
                .rounded(px(4.))
                .border_1()
                .border_dashed()
                .border_color(c(theme::LINE))
                .px(px(12.))
                .py(px(16.))
                .flex()
                .justify_center()
                .text_color(c(theme::FAINT))
                .child("No rules yet. Add one to highlight a word, a regular expression or a user.")
                .into_any_element()
        } else {
            div().mb(px(12.)).children(editors).into_any_element()
        };
        let tester = (!rules.is_empty()).then(|| self.rule_tester(&rules, window, cx));
        let add = btn(eid("hl-add"), "Add rule", BtnOpts { primary: true, small: true, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add_rule(cx)));

        div()
            .child(Self::heading_text("Your name"))
            .child(txt(11.).mb(px(4.)).text_color(c(theme::FAINT)).child("Twitch messages that mention your name are always highlighted in red."))
            .child(
                div()
                    .mb(px(16.))
                    .child(self.bool_row(&sv, "mentionSound", "Play a sound", None, false, false, cx))
                    .child(self.bool_row(&sv, "mentionFlash", "Flash the taskbar", Some("Only while Agu Chat isn't the active window"), false, true, cx)),
            )
            .child(Self::heading_text("Alert sound"))
            .child(
                div()
                    .mb(px(16.))
                    .child(Self::row("Default sound", Some("Used by rules and mentions that don't pick their own file"), sound_ctl, false))
                    .child(Self::row("Volume", None, volume, false))
                    .child(self.bool_row(&sv, "highlightSoundFocused", "Play sounds while Agu Chat is focused", Some("Off: sounds only play when you're in another window"), false, true, cx))
                    .children(sound_err.map(|e| txt(11.).mt(px(4.)).text_color(c(theme::RED_400)).child(e))),
            )
            .child(div().mb(px(8.)).mt(px(4.)).flex().items_center().justify_between().child(txt(13.).font_weight(gpui::FontWeight::SEMIBOLD).child("Rules")).child(add))
            .child(txt(11.).mb(px(8.)).text_color(c(theme::FAINT)).child("For every platform. Checked top to bottom: the first rule that matches colours the message. Your own messages are never highlighted."))
            .child(rules_body)
            .children(tester)
            .into_any_element()
    }
}

fn a_rgb(bg: (u8, u8, u8, f32)) -> gpui::Rgba {
    let mut x = gpui::rgb(((bg.0 as u32) << 16) | ((bg.1 as u32) << 8) | bg.2 as u32);
    x.a = bg.3;
    x
}

impl SettingsDialog {
    fn heading_text(t: &str) -> gpui::Div {
        txt(13.).mb(px(8.)).mt(px(4.)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(theme::TEXT)).child(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_suffix_is_not_shown() {
        assert_eq!(sound_label("My Ping-a9993e36.mp3"), "My Ping.mp3");
        assert_eq!(sound_label("plain.wav"), "plain.wav");
        assert_eq!(sound_label("x-ABCDEF12.wav"), "x-ABCDEF12.wav");
    }

    #[test]
    fn hex_colours() {
        assert!(hex_ok("#ff9f1a") && hex_ok("ff9f1a"));
        assert!(!hex_ok("#ff9f1") && !hex_ok("#gg9f1a"));
    }
}
