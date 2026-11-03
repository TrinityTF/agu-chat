# Agu Chat

<img src="build/icon.png" alt="Agu Chat" width="96" align="right" />

A desktop chat client for Windows, made for streamers and moderators. Open Twitch, Kick, YouTube, Rumble and X
chats side by side, or merge several platforms into one chat.

**Download:** grab `Agu-Chat-Setup-<version>.exe` from [Releases](https://github.com/TrinityTF/agu-chat/releases)
and run it. Agu Chat keeps itself up to date after that.

> This is a beta. Expect rough edges, and please report bugs in [Issues](https://github.com/TrinityTF/agu-chat/issues).

## Features

- **Tabs and splits:** as many tabs as you like, each with chats side by side or stacked. Drag to resize or rearrange.
- **Merged chats:** put channels from different platforms into one split, each message marked with its platform.
- **Emotes and badges:** Twitch, 7TV, BTTV and FFZ emotes, 7TV name paints, and each platform's own emotes and badges.
- **Moderation:** mod mode adds mod buttons to every message: ban, timeout and delete by default, or your own set (any timeout length, or a command such as `/warn {user} …`) in **Settings → Mod buttons**. Click a name for a user card with mod actions, and use slash commands such as `/ban`, `/timeout`, `/slow`, `/announce`, `/raid` and `/pin`.
- **Keyboard shortcuts:** change or add shortcuts in **Settings → Hotkeys**.
- **AutoMod:** approve or deny held messages right in chat.
- **Highlights:** colour messages that contain a word or phrase, match a regular expression, or come from a
  certain user, on every platform. Each rule can play a sound (a built-in ping or your own file) and flash the
  taskbar. Messages that mention your Twitch name are highlighted too.
- **Tab markers:** a tab you're not looking at shows a dot when new messages arrive, and lights up when one
  of them is a highlight.
- **Ignores:** hide messages from users you don't want to see, and hide or rewrite messages containing words or patterns you choose (Settings → Ignores, or "Ignore user" on a user card).
- **Search:** Ctrl+F in a split filters it to matching messages; `from:name` shows one person's messages.
- **Typing:** Tab completes emotes and names; `:` searches emotes, `@` mentions people, `/` lists commands; there's an emote picker, and Up brings back what you sent.
- **Recent history** loads when you open a Twitch channel, so a new split isn't empty.
- **Automatic updates** on the Beta or Stable channel.

## Platforms

| Platform | Read chat | Send messages | Moderate | Notes |
| --- | --- | --- | --- | --- |
| Twitch | Yes | Yes | Yes, where you're the broadcaster or a moderator | Log in once (see below). |
| Kick | Yes | Not yet | Not yet | Kick doesn't yet let desktop apps log in. |
| YouTube | Yes | Not yet | Not yet | Read-only in this beta. |
| Rumble | Yes | Yes | Yes, where Rumble allows your account | Log in with your Rumble account in Settings. Your password is only used to sign in and is never saved. |
| X | Yes | No | No | Chatters show as "X user 12345", because X only shows names to logged-in accounts. Paste the broadcast link to be sure the chat is found. |

## Logging in to Twitch

1. Open **Settings → Accounts** and click **Log in with Twitch**.
2. Open the link it shows, check the code matches, and approve on twitch.tv.

Your password never goes through Agu Chat, and you can log out or revoke access at any time
(Twitch → Settings → Connections).

## Using it

| To do this | Do this |
| --- | --- |
| New tab | `+` or Ctrl+T |
| Rename a tab | Double-click it |
| Close a tab | Middle-click it |
| Switch tabs | Ctrl+Tab, Ctrl+1…9 |
| Add a split | `⋯` on a split's header → Add split right / below |
| Resize splits | Drag the line between them |
| Move a split | Drag its header onto another split's edge |
| Merge splits | Drag a split's header (or a platform icon) onto the middle of another split |
| Close a split | Ctrl+W |
| Merge platforms | In the channel dialog, add several channels to one split |
| Split a channel back out | Click its platform icon on the split's header → Split out to the right / below, or drag the icon onto any split's edge |
| Mod mode | The sword button on a split's header |
| User card | Click a name: it opens in its own small window you can move anywhere, and several can be open |
| Always on top | The pin in the title bar, or Settings → Appearance and chat |
| All tabs | The arrow next to `+` lists every tab when they don't all fit |
| Search a split | Ctrl+F, then type words and/or `from:name`; Esc closes it |
| Ignore someone | "Ignore user" on their user card, or Settings → Ignores |
| New line in a message | Shift+Enter |
| Highlight rules and alert sounds | Settings → Highlights |
| Settings | Ctrl+P or the gear icon |
| Change shortcuts | Settings → Hotkeys: click `+` next to an action and press the keys |
| Change mod buttons | Settings → Mod buttons |

## Updates

In **Settings → Updates** choose a channel:

- **Beta:** every new build, so you get new features first.
- **Stable:** only fully released versions.

New versions download in the background and install when you close Agu Chat, or right away with **Restart to
update**. Switching from Beta to Stable keeps your current version until a newer stable one is out.

## Privacy

Your logins are stored only on your PC, encrypted with Windows' own protection, and are never sent anywhere except
to the platform they belong to. Settings and logs are in `%APPDATA%\Agu Chat\`.

## Building from source

```bash
npm install
```

```bash
npm run dev
```

```bash
npm test
```

`npm run dist` builds the Windows installer into `dist/`. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/RELEASING.md](docs/RELEASING.md).
