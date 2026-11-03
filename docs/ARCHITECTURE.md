
## Ignores and split search

Ignores are applied in the renderer (`chat/ignores.ts`) before rows are drawn, and ignored rows never alert
or mark a tab. Split search (Ctrl+F) is `chat/search.ts`.

# Agu Chat architecture

This is the contract between the pieces. Change it only when both sides change.

Agu Chat is an Electron app. The **main process** (Node, ESM) connects straight
to each platform and acts as an in-process "local relay": it speaks the same
frames the agu.lol chat page already understands, so renderer code ported from
`modtools.agu.lol/web/src/app/pages/Chat.tsx` (and `KickChat.tsx`,
`YouTubeChat.tsx`, `RumbleChat.tsx`, `XChat.tsx`) keeps working with small
changes. Nothing talks to agu.lol or api.agu.lol.

```
renderer (React, sandboxed)  <--  window.agu (preload, contextBridge)  -->  main (Node)
  notebook / splits / chat                 IPC                       relay: sources + api router
                                                                     platforms: twitch kick youtube rumble x
                                                                     store: settings.json + secrets (safeStorage)
                                                                     updater: electron-updater, beta/stable
```

## Layout of the repo

| path | what |
| --- | --- |
| `src/main/main.js` | app lifecycle, the window, IPC wiring |
| `src/main/store.js` | settings (`settings.json`) and secrets (`secrets.bin`, encrypted with `safeStorage`) |
| `src/main/relay.js` | `SourceManager` (ref-counted live sources) and the `api` router |
| `src/main/platforms.js` | the list of platform modules |
| `src/main/<platform>/index.js` | one module per platform (see "Platform module") |
| `src/main/<platform>/vendor/` | files copied from `modtools.agu.lol/render/lib/` (render commit `7704aec`), each with a header naming its origin and what was changed |
| `src/renderer/icons.tsx`, `platformIcons.tsx`, `hooks.ts` | shared by `layout/` and `chat/` |
| `src/renderer/chat/ChatPane.tsx` | the one component `layout/` renders per split (props are the contract) |
| `src/renderer/chat/highlights.ts` | highlight rule matching (pure, tested by `npm test`) |
| `src/renderer/alerts.ts` | highlight alerts: sound (Web Audio) and taskbar flash, deduplicated across splits |
| `src/main/highlightRules.js`, `src/main/sounds.js` | validation of the `highlightRules` setting; custom alert sounds in `userData/sounds` |
| `src/main/updater.js` | GitHub Releases updates, beta/stable channel |
| `src/preload/preload.cjs` | the `window.agu` bridge (CommonJS: sandboxed preloads cannot be ESM) |
| `src/renderer/` | the React app (Vite root). `layout/` = notebook, splits, dialogs. `chat/` = message view, input, user card |
| `src/renderer/bridge.ts` | typed access to `window.agu`, plus `relayApi()` |
| `src/renderer/protocol.ts` | the shared types below, in TypeScript |

## Sources

A **source** is one platform's chat for one channel:

```ts
type Platform = "twitch" | "kick" | "youtube" | "rumble" | "x";
type SourceRef = { platform: Platform; channel: string };
```

`channel` is what the user typed: a Twitch login, a Kick slug, a YouTube
`@handle` / channel id / video URL, a Rumble channel or video URL, an X
username. Each platform module normalizes it (`normalizeChannel`), and the
source key is `${platform}:${normalized}`.

The renderer calls `agu.subscribe(source)` and gets a `subId`. Frames for that
subscription arrive through `agu.onFrame((subId, frame) => ...)`. Several splits
showing the same channel share ONE upstream connection: `SourceManager`
ref-counts subscribers, replays the current snapshot to each new subscriber,
and stops the source `LINGER_MS` (30s) after its last subscriber leaves.

### Frames

Every frame is a plain object with a `type`. Each platform emits **the same
frames its agu.lol relay socket sends today**, so the ported row renderers keep
working:

- Twitch: what `/ws/chat` sends (`render/lib/ircRelay.js`): `chatlog` hello
  (`messages`, `pinned`, `modfeed`, `automod`, `suspicious`, `esub`, ...),
  `chat`, `chatnotice`, `clearchat`, `clearmsg`, `pin`, `modfeed`, `automod`,
  `suspicious`, `esub`, ... Messages carry `platform: "twitch"`.
- Rumble: what `/ws/rumble` sends: `chatlog`, `chat`, `status`, `delete`,
  `mute`, `pin`, `emotes`.
- Kick, YouTube, X: what `/ws/kick`, `/ws/youtube`, `/ws/x` send (see
  `render/server.js` and each `render/lib/<platform>.js`).

On top of that, **every source emits a uniform connection frame**, so a split
header can show state without knowing the platform:

```ts
type ConnFrame = {
  type: "conn";
  state: "connecting" | "open" | "offline" | "error" | "unavailable";
  detail?: string;      // human readable: "Chat is disabled for this live stream.", "Not live", ...
  live?: boolean;       // when the platform knows
  title?: string;       // stream title, when known
  canSend?: boolean;    // the logged-in account can post here from this app
  readOnlyReason?: string; // why not, in one line, shown in the input box
  channelId?: string;   // Twitch only: broadcaster id (also on the chatlog hello)
};
```

## The api router (request / response)

`agu.api(source, path, init)` → `{ status, body }`. The paths and bodies are
the agu.lol relay's HTTP API (`render/server.js`), with one difference: the
channel comes from `source`, not from a session. Platform modules register
their own routes. The renderer's `relayApi(source, path, init)` wraps this in a
`Response`-like object (`ok`, `status`, `json()`), so ported code that did
`const r = await relayApi("/api/mod/action", {method: "POST", body: JSON.stringify(x)})`
keeps its shape with a `source` added.

Twitch routes (same semantics as `render/server.js`): `/api/chat/send`,
`/api/mod/action`, `/api/mod/caps`, `/api/mod/user`, `/api/mod/automod`,
`/api/chat/settings`, `/api/chat/shield`, `/api/chat/announce`,
`/api/chat/shoutout`, `/api/chat/raid`, `/api/chat/terms`, `/api/chat/rules`,
`/api/chat/badges`, `/api/chat/twitch-emotes`, `/api/chatters`.
Pins go through `/api/mod/action` (`pin` / `unpin`) as on the site.

Rumble: `/api/rumble/send`, `/api/rumble/action`, `/api/rumble/caps`.
Kick: `/api/kick/send`, `/api/kick/action`, `/api/kick/caps` (only if sending works, see README).

An unknown path answers `{ status: 404, body: { error: "not found" } }`.

## Accounts

`agu.accounts.get()` → `{ twitch, kick, youtube, rumble, x }`, each
`{ connected: boolean, user?: { id, login, name }, scopes?: string[], pending?: DevicePrompt, error?: string }`.
Never a token: tokens stay in main.

- `agu.accounts.login("twitch")` starts the OAuth **device code flow** and
  resolves with `{ userCode, verificationUri, expiresAt }`. Main polls in the
  background and emits `accounts` changes. `agu.accounts.cancel("twitch")`.
- `agu.accounts.login("rumble", { username, password })` may resolve
  `{ twoFactor: true }`; then `login("rumble", { code })` finishes it.
- `agu.accounts.logout(platform)`.

## Settings

`agu.settings.get()` / `agu.settings.set(patch)` / `agu.settings.onChange(cb)`.
The `Settings` type and defaults live in `src/renderer/protocol.ts` and
`src/main/store.js` (keep them in sync). Notable keys: `twitchClientId` (the
user's own Twitch app, Public client type), `updateChannel` (`"beta"` |
`"stable"`), the chat display options, `hotkeys` (action id → bindings, only the
actions the user changed; actions and defaults in `src/renderer/layout/hotkeys.ts`) and `modButtons` (the
mod-mode row buttons in order; defaults and validation in `src/renderer/chat/modButtons.ts`), the highlight
settings (`highlightRules` and the alert options) and the ignore lists `ignoredUsers` (`{ platform, login }[]`)
and `ignoredPhrases` (`{ pattern, regex, caseSensitive, action, replacement }[]`).

Main validates every value, on `set` and when loading `settings.json`, in `clean()` (`src/main/store.js`):
keys with a sanitizer in `SANITIZE` (hotkeys, mod buttons, highlight rules, volume, the ignore lists; the ignore
sanitizers live in `src/main/settingsSanitize.js`) get a shape check that drops malformed entries; every other
key must have its default's type. The renderer validates the meaning when it reads them. `npm test` runs the
`node --test` suites in `test/`.

## Highlights and alerts

Highlight rules are matched in the renderer, per row, for every platform (`chat/highlights.ts`); the first
matching rule colours the row. A row that arrived in a `chatlog` frame is marked `backlog` by the chat store:
history is coloured but never alerts or marks a tab. For live rows `ChatPane` calls `onActivity("message" |
"highlight")` (the notebook marks unselected tabs) and `alertOnce` (`alerts.ts`), keyed by message, so the
same channel open in two splits alerts once.

- `agu.app.flash()` flashes the taskbar button (`BrowserWindow.flashFrame`) unless the window is focused;
  focusing the window stops it.
- `agu.sounds.pick()` opens a file dialog and copies the chosen audio file into `userData/sounds`, resolving
  `{ file }` (a bare file name, stored in settings) or `null`. `agu.sounds.read(file)` returns its bytes; only
  bare names inside that folder are accepted. "" in a sound setting means the built-in ping, which is
  synthesized in the renderer.
`"stable"`), the chat display options, and the ignore lists `ignoredUsers`
(`{ platform, login }[]`) and `ignoredPhrases` (`{ pattern, regex, caseSensitive, action, replacement }[]`).
Main validates every value against `DEFAULT_SETTINGS`: a scalar must have the default's type, and a list
needs a sanitizer in `src/main/settingsSanitize.js` (malformed entries are dropped, also when loading).
Ignores are applied in the renderer (`chat/ignores.ts`) before rows are drawn; split search is
`chat/search.ts`. Pure logic like this is tested with `npm test` (`node --test`, files in `test/`).

## Layout

`agu.layout.get()` / `agu.layout.set(layout)` persist the notebook to
`layout.json` in userData:

```ts
type SplitNode =
  | { kind: "leaf"; id: string; sources: SourceRef[]; modMode: boolean }
  | { kind: "row" | "col"; id: string; children: SplitNode[]; sizes: number[] }; // sizes are fractions summing to 1
type Tab = { id: string; title: string; root: SplitNode | null };
type Layout = { tabs: Tab[]; active: string };
```

A leaf with one source is a single channel; with several it is a merged
co-chat (messages interleaved by time, each row marked with its platform).

## Platform module

`src/main/<platform>/index.js` exports:

```js
export const platform = "kick";
export function normalizeChannel(input) -> string            // throws Error(userMessage) if unusable
export function createSource({ channel, emit, ctx }) -> {
  start(),                 // open upstream; emit({type:"conn", state:"connecting"}) first
  stop(),                  // close everything, clear timers
  snapshot() -> Frame[],   // frames a NEW subscriber needs (usually one chatlog + the latest conn)
}
export const routes = { "POST /api/kick/send": async ({ source, body, ctx }) => ({ status, body }) }  // optional
export const accounts = { state(), login(args), cancel(), logout() }  // optional
export function init(ctx) {}  // optional, called once at startup
```

`ctx` gives a module what it may use from the app: `ctx.settings.get()`,
`ctx.secrets.get(key)` / `set(key, value)` / `delete(key)`,
`ctx.accountsChanged()`, `ctx.log(...)`, `ctx.userAgent` and
`ctx.platforms.twitch.getUserToken()` (other modules may need it).

## Rules that carry over from render/README.md

- Rumble: only `wn0.rumble.com` (then `web7` for chat), never the apex. Never
  try to get past a Cloudflare challenge: no TLS impersonation, no scripted
  browsers, no browser user agent strings.
- Kick: send our own honest product UA (`ctx.userAgent`), never a browser UA.
- YouTube: one poll per source, honour `timeoutMs`, back off on failure.
- X: guest token only, read-only, no paid v2 calls, no bot token in the app.
- Twitch: device code flow with the user's own **Public** client id. Never use
  another app's client id (twitch.tv's own web client id included).
