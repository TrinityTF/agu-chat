//! macOS application menu and the Mac-convention shortcuts that live in it: Cmd+Q quit, Cmd+H hide, Cmd+Opt+H hide
//! others, Cmd+M minimise. (Cmd+, opens Settings through the app's own hotkey table, see layout/hotkeys.rs; the
//! menu's "Settings…" item is the same thing for the mouse.) The Edit menu is there for macOS's sake as well: AppKit
//! adds "Emoji & Symbols" (Ctrl+Cmd+Space) and dictation only to an app's Edit menu.
//! Windows has no menu bar, so this module only exists on macOS.
use gpui::{actions, App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType};

use crate::layout::app::{AguApp, Dialog};
use crate::text_input;

actions!(agu_app_menu, [Quit, Hide, HideOthers, ShowAll, Minimize, Zoom, OpenSettings]);

/// Registers the actions, their key bindings and the menu bar (call once at startup, after the keymap is set up).
pub fn install(cx: &mut App) {
    cx.on_action(|_: &Quit, cx: &mut App| {
        // Same as "Restart and update": the layout is written out before the process ends.
        for w in cx.windows() {
            if let Some(h) = w.downcast::<AguApp>() {
                let _ = h.update(cx, |app, _, _| app.flush_layout());
            }
        }
        cx.quit();
    });
    cx.on_action(|_: &OpenSettings, cx: &mut App| {
        let target = cx
            .active_window()
            .and_then(|w| w.downcast::<AguApp>())
            .or_else(|| cx.windows().into_iter().find_map(|w| w.downcast::<AguApp>()));
        if let Some(h) = target {
            let _ = h.update(cx, |app, window, cx| {
                window.activate_window();
                if app.dialog.is_none() {
                    app.open_dialog(Dialog::Settings, window, cx);
                }
            });
        }
    });
    cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx: &mut App| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx: &mut App| {
        if let Some(w) = cx.active_window() {
            let _ = w.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &Zoom, cx: &mut App| {
        if let Some(w) = cx.active_window() {
            let _ = w.update(cx, |_, window, _| window.zoom_window());
        }
    });

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);

    cx.set_menus(vec![
        Menu {
            name: "Agu Chat".into(),
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Agu Chat", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Agu Chat", Quit),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Cut", text_input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", text_input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", text_input::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", text_input::SelectAll, OsAction::SelectAll),
            ],
        },
        Menu { name: "Window".into(), items: vec![MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)] },
    ]);
}
