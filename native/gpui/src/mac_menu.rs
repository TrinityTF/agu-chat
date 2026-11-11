//! macOS application menu and the Mac-convention shortcuts that live in it: Cmd+Q quit, Cmd+H hide, Cmd+Opt+H hide
//! others, Cmd+M minimise. (Cmd+, opens Settings through the app's own hotkey table, see layout/hotkeys.rs.)
//! Windows has no menu bar, so this module only exists on macOS.
use gpui::{actions, App, KeyBinding, Menu, MenuItem, SystemMenuType};

use crate::layout::app::AguApp;

actions!(agu_app_menu, [Quit, Hide, HideOthers, ShowAll, Minimize, Zoom]);

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
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Agu Chat", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Agu Chat", Quit),
            ],
        },
        Menu { name: "Window".into(), items: vec![MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)] },
    ]);
}
