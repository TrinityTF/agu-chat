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

## Installer and updates (native app)

The native app ships as its own NSIS installer and updates itself from GitHub Releases of `TrinityTF/agu-chat`,
next to the Electron app's releases. The two never see each other:

| | Electron (`release.yml`) | Native (`release-native.yml`) |
| --- | --- | --- |
| tag | `v<version>` (semver) | `native-v<version>` (not semver) |
| assets | `Agu-Chat-Setup-<version>.exe`, `latest.yml`, `.blockmap` | `Agu-Chat-Native-Setup-<version>-x64.exe`, `...-x64.exe.sha256` |
| release type | beta = prerelease, stable = "latest" | beta = prerelease, stable = full release, **never "latest"** |
| triggers | push to `main`, manual | manual, or pushing a `native-v*` tag. Never a branch push |
| read by | electron-updater | `native/core/src/updater.rs` |

- electron-updater (beta) walks `releases.atom` and skips tags that are not valid semver, so `native-v...` is invisible
  to it; electron-updater (stable) reads `/releases/latest`, which is why a native release is never marked latest.
- The native updater (`Updater`, `GithubFeed`, `parse_releases`) pages through the releases list (3 x 100) and keeps
  only releases tagged `native-v<semver>` that carry `Agu-Chat-Native-Setup-<version>-x64.exe`; drafts, Electron
  releases and half-uploaded native releases are dropped. Channels are unchanged: beta = newest of prereleases and
  full releases, stable = full releases only, never a downgrade (rules in `docs/RELEASING.md`, same code path).
- `NativeInstaller` downloads that asset to `<data dir>\updates`, checks the size and the sha256 (GitHub's `digest`
  field, else the `.sha256` asset), and only then reports `ready`. "Restart and update" runs
  `Agu-Chat-Native-Setup-<version>-x64.exe /S /R` detached and quits the app (the quit goes through
  `Backend::set_quit_hook`, so the UI flushes `layout.json` first). The installer waits for `Agu Chat.exe` to be
  released, replaces it, and starts the new copy (`/R`).
- Version: `AGU_APP_VERSION` (runtime) > `AGU_BUILD_VERSION` (set by the workflow at build time) > Cargo version.
  The base version of a release is `native/gpui/Cargo.toml`; beta = `<base>-beta.<run number>`, stable = `<base>`.
- Dev/test aid: `AGU_RELEASES_URL=http://127.0.0.1:<port>/releases` points the feed at a local fake (loopback only).

### Installer (`native/installer/`)

NSIS (the same tool electron-builder uses): `agu-chat.nsi`, built by `build-installer.ps1 -Version <v>` from
`target\release\agu-chat-gpui.exe` (it also writes the `.sha256`). `makensis` is found through `NSIS_HOME`, `PATH`,
Program Files, or the copy electron-builder caches in `%LOCALAPPDATA%\electron-builder\Cache`; CI installs it with
`choco install nsis`. `make-icon.ps1` makes `app.ico` from `build/icon.png`.

Same identity as the Electron installer so it replaces it in place: per-user (no admin), default folder
`%LOCALAPPDATA%\Programs\agu-chat`, `Agu Chat.exe`, `Uninstall Agu Chat.exe`, the same uninstall registry key
(`d7530026-c2b3-572b-a28e-234d84faefc5`), Start-menu and desktop shortcut `Agu Chat`. Installing over an Electron
install removes its runtime files (`locales`, `resources`, `*.pak`, `*.dll`, ...) and keeps `%APPDATA%\Agu Chat`
(settings, layout, logins). Flags: `/S` silent, `/D=dir`, `/R` start the app afterwards, `/NOWAIT`.
`build-installer.ps1 -Test` builds a variant with its own uninstall key and shortcut name that defaults to a folder
in `%TEMP%`: that is what to try, never the real one, on a machine that has an install.

## Settings dialog, sounds, memory (round 3B)

- `layout/settings_dialog.rs` + `layout/highlights_section.rs` + `layout/settings_widgets.rs` port
  `SettingsDialog.tsx` / `HighlightsSection.tsx`. Every control calls `settings_set(patch)` with the same camelCase
  keys as Electron (optimistic local merge, then the bridge's answer), so `settings.json` stays compatible both ways.
  The DOM controls GPUI lacks are drawn by `settings_widgets.rs` (`<select>` as a button plus a list, a range slider
  with drag, checkboxes); text fields are `TextInput`s kept by key and re-synced from the saved value.
  Two deliberate differences: the update progress bar shows the real percentage (the backend reports 0..1, the JS
  printed `round(0..1)`), and the highlight colour is a palette + hex field instead of the OS colour picker.
- Screenshot aid: `AGU_SHOT=settings:<accounts|appearance|highlights|ignores|hotkeys|modButtons|updates|about>`.
  The Electron reference is `src/renderer/layout/dev/settings.html?s=<section>` on a vite dev server (dev mock
  bridge, same data as `layout/dev_mock.rs`).
- Sounds: "Choose file..." is GPUI's `prompt_for_paths`, then `sounds_import` copies the file into `<data dir>/sounds`;
  the speaker button plays the file (or the built-in ping) through the same `Alerts::play` the chat view uses.
- Image memory: decoded emotes/badges (all frames, CPU BGRA buffers in `RenderImage`) used to stay forever. The core
  image cache now has a budget (`BUDGET_BYTES` = 160 MB of decoded frames, `AGU_IMAGE_BUDGET_MB` overrides it): past
  it the least recently used images that were not asked for in the last 3 s are dropped, the GPUI side frees their
  atlas tiles (`Window::drop_image`), and they are fetched again if they come back. An animation over 8 MB decoded
  keeps only its first frame. Layout reads an image's natural size from a separate map that survives eviction, and
  layouts are keyed by the monotonic load count, so an eviction never changes a row's height or relayouts the list.
  (64 MB was tried and rejected: the busy fake chat's working set is above it, so images were re-fetched all the time
  and CPU doubled.) The chat store already caps rows (1000, 1600 while scrolled up).
