# GPUI port (internal)

Agu Chat is being rebuilt as a native Rust app on GPUI (`native/`), replacing Electron. Goal: **the same
app**: identical look, layout, behaviour, settings and files. Only the engine changes. The benchmark that
motivated it lives in `native/bench/README.md`.

## Shape

```
native/core  (crate agu-chat-core)   = port of src/main      backend: relay, platforms, store, updater
native/gpui  (crate agu-chat-gpui)   = port of src/renderer  UI: notebook, splits, chat, dialogs
native/core/src/protocol.rs          = port of src/renderer/protocol.ts: THE contract (AguBridge trait)
```

- **Port module by module, 1:1.** Every Rust file starts with `//! Port of <js/ts path>` and keeps the JS
  structure, names (snake_cased) and comments where they still apply, so parity can be checked file against
  file. Behaviour differences need a comment saying why.
- **Frames, api bodies and account args stay JSON** (`serde_json::Value`) exactly as the Electron app's,
  across the bridge. Inside the UI, the chat store converts frames into typed rows once on arrival.
- **Same files, same place:** `%APPDATA%\Agu Chat\` (`settings.json`, `layout.json`, `window.json`,
  `secrets.bin`, `sounds\`, `agu-chat.log`). An existing install's settings, layout and logins carry over.
  `secrets.bin` stays readable and writable in Electron's `safeStorage` format (Chromium os_crypt: AES-256-GCM,
  `v10` prefix, key = DPAPI-unwrapped `os_crypt.encrypted_key` from `Local State`).
- **Look:** the theme tokens in `src/renderer/index.css` and the Tailwind classes in each component map 1:1
  to GPUI's tailwind-style methods (`px_2()`, `text_xs()`, `rounded_md()`, ...). Use the token constants,
  never raw hex. Font: Segoe UI (what Chromium's `system-ui` resolves to on Windows). Icons: the SVGs from
  `src/renderer/icons.tsx` / `platformIcons.tsx` as files in `native/gpui/assets/icons/`.
- **Async:** the backend owns a tokio runtime; `AguBridge` futures are executor-agnostic, so the UI
  `.await`s them inside GPUI tasks. Bridge callbacks fire on backend threads; the UI hops to its thread.
- **Dev without a backend:** `native/gpui` has a mock bridge (port of `layout/devMock.ts`) behind
  `AGU_MOCK=1`, so the UI runs and can be screenshotted on its own.
- **Tests:** the pure-logic suites in `test/` are ported to `cargo test` next to the code they test.
- **Rules from ARCHITECTURE.md still hold** (Rumble hosts, honest UAs, X guest only, Twitch Public client id).
  Live checks only on the user's own channel `a_g_u__`; busy load comes from `native/bench/fake-irc.mjs`.

## Toolchain

MSVC (pinned by `native/rust-toolchain.toml`), Windows SDK 10.0.26100, `gpui = "=0.2.2"` (pinned: GPUI's API
moves with Zed; upgrade deliberately). Cargo lives in `%USERPROFILE%\.cargo\bin`.

## Rounds

1. Core foundation + Twitch (relay, router, store/secrets, accounts, twitch/*) ‖ UI shell (window chrome,
   notebook tabs, split tree, split header, channel dialog, theme, icons, hotkeys, mock bridge).
2. Kick / YouTube / Rumble / X backends ‖ chat view (ChatPane, MessageList, rows, chat store, emotes,
   cosmetics, badges, highlights, ignores, search, pinned card, toasts, alerts).
3. Input (SendBox, autocomplete, emote picker, slash commands), user card, mod popover and buttons, channel
   modes ‖ settings dialog (all sections), sounds, updater + installer + release workflow.
4. Side-by-side visual parity pass against the Electron app, screen by screen.

The Electron app stays the shipping app on `main` until the port reaches parity.
