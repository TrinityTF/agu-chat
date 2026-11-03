# Agu Chat

A Windows desktop chat client for streamers and mods, in the spirit of Chatterino: notebook tabs, resizable
splits, dense message rows, mod mode and user cards. It reads Twitch, Kick, YouTube, Rumble and X, and can put
several platforms into one merged split.

Agu Chat connects straight to each platform from your PC. It does not need agu.lol or any server of ours.

Version: **0.0.1-beta.1** (the first beta, "v0.01").

## Platforms

| Platform | Read | Send | Moderate | How it connects | Why the limits |
| --- | --- | --- | --- | --- | --- |
| **Twitch** | Yes | Yes, once logged in | Yes (ban, timeout, delete, warn, pin, AutoMod, chat modes, raids, shoutouts, announcements) where you are broadcaster or mod | Anonymous IRC over WebSocket for reading (one shared connection), recent-messages.robotty.de for history, EventSub WebSocket and Helix with your own login | Login uses the OAuth device code flow with **your own Twitch app** (Public client, no secret). Chat rules are not shown: Helix has no endpoint for them. |
| **Kick** | Yes | No | No | Kick's public Pusher WebSocket (the same one kick.com's chat uses), history from Kick's channel API | Kick's OAuth requires a `client_secret` for the token exchange, which a desktop app cannot keep secret. Read-only until Kick offers a public-client flow. |
| **YouTube** | Yes | No | No | YouTube's own web (InnerTube) live-chat endpoints, polled at the interval YouTube asks for | Sending through the official API costs quota from one shared 10,000-unit daily budget and needs Google's verification. Read-only in this beta. |
| **Rumble** | Yes | Yes, with your Rumble login | Yes (timeout, ban, unban, delete), as Rumble allows your account | `wn0.rumble.com` (and `web7` for chat) Server-Sent Events. Login through Rumble's `service.php`. | Your password is used once to log in and never stored; only the session is kept, encrypted. Rumble's main domain is behind a bot challenge, which Agu Chat never tries to get past. **Sending and moderating are not verified yet** (needs a real account). |
| **X** | Yes | No | No | X's live-chat stream with a guest token (no account) | Sending needs X's paid API. Names need a login, so chatters show as `X user 12345`. A username only finds a live broadcast if the streamer tweeted its link; pasting the broadcast link always works. |

Messages from your own Twitch account show up through the same chat stream as everyone else's.

## Twitch setup (one time)

Agu Chat logs in with your own Twitch app, so nobody else's app or secret is involved:

1. Go to <https://dev.twitch.tv/console/apps> and choose **Register Your Application**.
2. Name: `Agu Chat (<your name>)` (the name must be unique on Twitch).
3. OAuth Redirect URL: `http://localhost` (the form requires one; the app does not use it).
4. Category: **Chat Bot**. Client Type: **Public**. Create it.
5. Open **Manage**, copy the **Client ID**, and paste it into Agu Chat's **Settings → Accounts**.
6. Click **Log in with Twitch**, open the link it shows and enter the code.

## Using it

- **Tabs:** `+` or Ctrl+T adds a tab. Double-click a tab to rename it. Middle-click closes it. Ctrl+Tab, Ctrl+1…9 switch tabs.
- **Splits:** use the `⋯` menu on a split header to add a split to the right or below. Drag the thin dividers to resize. Drag a split header onto another split's edge to move it. Ctrl+W closes the focused split.
- **Merged co-chat:** in the channel dialog, add several channels (any mix of platforms) to one split.
- **Mod mode:** the sword button on a split header shows ban, timeout and delete buttons on every row.
- **User cards:** click a name. Cards can be dragged and several can be open.
- **Input:** Enter sends, Shift+Enter makes a new line, Up recalls what you sent. Tab completes emotes and names, `:` searches emotes, `@` mentions, `/` lists commands (`/ban`, `/timeout`, `/slow`, `/announce`, `/raid`, `/pin` and more).
- **Settings:** Ctrl+P or the gear icon.

## Updates

Agu Chat updates itself from GitHub Releases. In **Settings → Updates** pick a channel:

- **Beta:** every prerelease. Newest features, first to break.
- **Stable:** full releases only.

A new release on your channel downloads in the background and installs when you close the app, or right away
with **Restart to update**. Moving from Beta to Stable never downgrades you: you stay on your build until a newer
stable release exists. See [docs/RELEASING.md](docs/RELEASING.md) for how releases are cut.

## Development

```bash
npm install
```

```bash
npm run dev
```

`npm run dev` starts Vite for the UI and opens Electron against it. `npm run dist` builds the Windows installer
into `dist/` without publishing.

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how the main process, the bridge and the UI fit together.
- [docs/RELEASING.md](docs/RELEASING.md): channels, versions and the release workflow.
- [docs/chatterino-ideas.md](docs/chatterino-ideas.md): features Chatterino has that Agu Chat does not yet.

Platform code in `src/main/<platform>/vendor/` was copied from the agu.lol relay (`modtools.agu.lol/render/lib`)
and the chat view from its web chat page. Each file's header says where it came from and what changed.

Settings, layout and logs live in `%APPDATA%\Agu Chat\`. Logins are stored in `secrets.bin`, encrypted with
Windows DPAPI through Electron's `safeStorage`, so the file is useless to another Windows user or machine.

Chatterino (MIT) is the reference for the look and feel; no Chatterino code, name or logo is used.
