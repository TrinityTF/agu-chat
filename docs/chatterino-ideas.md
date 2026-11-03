# Chatterino features Agu Chat does not have yet

Reference: [Chatterino 2](https://github.com/Chatterino/chatterino2) (MIT license) at 2.5.5, read from its
settings (`src/singletons/Settings.hpp`), settings pages (`src/widgets/settingspages`), controllers
(`src/controllers/*`: highlights, ignores, filters, hotkeys, commands, notifications, logging, nicknames, userdata,
plugins), dialogs and `CHANGELOG.md`. This is a feature review only: ideas are described in our own words and no
Chatterino code is copied or planned to be.

Agu Chat beta already has: notebook tabs, splits (side by side / stacked, resize, move), merged multiplatform
splits, mod-mode toggle with inline ban/timeout/delete, user cards with mod actions, slash commands, Tab / colon /
@ autocomplete, emote picker, Twitch/7TV/BTTV/FFZ emotes and badges, 7TV paints, own-name mention highlight,
recent-messages backlog, AutoMod allow/deny, pinned messages, auto-updates, highlight rules (phrase / regex /
user, per-rule colour, case toggle) with per-rule sound and taskbar flash, tab unread / highlight markers.

Line format: **feature** — value for Agu Chat — effort (S = a day or less, M = a few days, L = a week or more).
`[Twitch]` = only meaningful on Twitch (other platforms lack the API). `★n` = in the top 10 below.

## Top 10 to do next

1. ★1 Highlights with phrase/regex/user rules (M)
2. ★2 Highlight sounds, taskbar flash and toasts (M)
3. ★3 Ignores (users and phrases, with replacement) (M) — done
4. ★4 Split search, Ctrl+F (S) — done (filter only; match highlighting and "search all splits" are still open)
5. ★5 Tab highlighting on mentions and new messages (S)
6. ★6 Keyboard shortcuts and a hotkeys editor (M)
7. ★7 Customizable mod buttons (M)
8. ★8 Custom commands / aliases (M)
9. ★9 Live notifications and a live indicator on tabs (M)
10. ★10 Reply thread view (M)

Why these: highlights, sounds, ignores and tab highlighting are what makes a busy multi-split window usable
(you only look where something happened); search, hotkeys and commands are the daily mod/power-user speed-ups;
live alerts and reply threads fit Agu Chat's multiplatform pitch (one window to follow several streamers/chats).
Runners-up if time allows: streamer mode, split filters, link/image previews, nicknames, logging.

## Highlights, notifications and attention

- ★1 **Highlights: phrases, regex, usernames, badges, with per-rule color and case/whole-word options** — you see the messages that matter (your name variants, a game keyword, a friend) in a fast chat; Chatterino also colors first-time chatters, subs/resubs, redeems, announcements, whispers, AutoMod-caught, watch streaks and your own messages — M
- ★2 **Highlight sounds (global default and per-rule custom file), taskbar flash and desktop toast, with "play even if the window is focused"** — streamers/mods who look away from chat still get pinged; toast click can open the channel — M
- **Highlight blacklist (users who never trigger highlights)** — stops a spamming bot from ringing the bell all day — S
- ★5 **Tab highlighting: unread dot and a stronger color for a mention/highlight, optionally per split** — in a notebook with many channels you see which tab needs you; Chatterino also marks the tab live — S
- ★9 **Live notifications: alert (sound/toast/flash) when a watched channel goes live, plus an indicator on the tab and a "live only" tab filter** [Twitch first; Kick/YouTube/Rumble/X where a live check exists] — mods/viewers following several streamers don't need a separate tab open — M
- **Followed-channels window (who is live from the account's follows)** [Twitch] — quick way to open a split for someone who just started — M
- **System-message options: show joins/parts, subscription/raid/announcement lines, first-message markers** — control over noise in large chats — S
- **Mentions tab (all highlighted messages from every split in one list)** — one place to catch everything you missed; Chatterino has a mentions channel — M

## Filtering and noise control

- ★3 **Ignored users and ignored phrases (plain/regex), with "hide the message" or "replace the matched text with ..."** — removes bots and spam without a mod action; Chatterino can also sync Twitch's blocked-users list and show/hide blocked users — M
- **Split filters: a small expression (author, badges, message text, flags such as highlighted/first message/redeem/whisper, channel live/name) that decides what a split shows** — build a "mods only", "subs only", "my mentions" or "no commands" split from the same channel; Chatterino has a full filter language with a picker per split — L (a simplified rule builder is M)
- **Hide similar/repeated messages (by same user or all users, similarity %, within N seconds)** — kills copy-paste spam waves — M
- **Hide moderated (deleted/timed-out) messages entirely, and a deleted-message character limit** — choose between struck-through text and a clean feed — S
- **Allow/disallow duplicate messages** — Twitch drops your own repeated messages; useful as a setting — S
- **Message batching / throttled rendering for busy chats** — keeps the UI smooth in a 50k-viewer chat; check virtual-list limits before it becomes a problem — M

## Search, navigation and keyboard

- ★4 **In-split search (Ctrl+F) with matching highlights, and "search all splits"** — find "who said X" or a user's last messages without leaving the window; Chatterino's search accepts the filter syntax and user:/channel: shortcuts — S
- ★6 **Keyboard shortcuts for everything: new tab/split, close, reopen closed split/tab, select tab 1-9, next/previous tab, focus neighbouring split, move tab, scroll page/top/bottom, zoom, open settings** — power users and mods stay off the mouse; Chatterino also remembers tab visit history (back/forward) — M
- ★6 **Hotkeys editor (settings page listing actions by category, rebind, reset)** — lets users fit shortcuts to their habits; also hotkeys for mod actions and "run command" — M
- **Quick switcher (Ctrl+K): type a tab/channel name to jump** — fast navigation with 20+ tabs — S
- **Reopen closed split / tab** — recover from an accidental close without re-adding the channel — S
- **Zoom in/out/reset (Ctrl +/-/0) and UI scale** — accessibility, 4K and small screens — S
- **Input box history (Up/Down through sent messages, incremental search)** — re-send or fix a previous message — S
- **Keep message in input after sending (hotkey variant), cancel-reply key** — faster repeat and reply flows — S
- **Scrollback pause on hover (and the "pause on hover" delay setting), smooth scrolling, mouse scroll multiplier** — readable chat while it moves — S
- **Lock notebook layout (no accidental drag/close)** — protects a carefully built layout on stream — S
- **Sort tabs alphabetically, tab direction (top/left/right/bottom), show/hide tab close button, "only live tabs" visibility** — tab bar tidying for many channels — S

## Moderation

- ★7 **Customizable mod buttons in the split header/messages: user-defined timeout lengths, ban, unban, delete, and arbitrary commands with a custom icon/label, plus a separate mod-actions list per channel** — each moderator tunes the buttons to their rules (e.g. 1m, 10m, 1d, "warn"); Chatterino stores them per channel — M
- **Moderation / mod view popout (open the platform's mod view in the browser) and the unban-request queue** [Twitch] — jump to full mod tools from the split — S
- **AutoMod queue view (all held messages in one panel) and low-trust-user highlighting/command (/lowtrust, /monitor, /restrict)** [Twitch] — mods can process held messages and suspicious users without hunting in the feed — M
- **Chat mode controls and commands (slow, followers, subs, emote-only, unique chat, shield mode with undo)** — one-click chat mode changes; many already work as slash commands, so a header menu is the gap — S
- **Warn, announce (colors), shoutout, raid/unraid, vip/unvip, mod/unmod, commercial, set title/game, marker, clip** [Twitch] — covers the rest of the broadcaster/mod toolbox from the input box — M
- **Polls and predictions: create/end/cancel/lock/resolve from commands, and show live state** [Twitch] — broadcaster flow without leaving chat; Chatterino 2.5.5 added the commands only — M
- **Streamer mode (auto-detect OBS/streaming software or manual toggle): hide mod actions, mute mention sounds, hide links/whispers/sensitive info** — safe to use on stream; has a hotkey — M
- **Shield mode and "delete message" confirmation options** — avoids mis-clicks in a raid — S

## Messages and display

- ★10 **Reply threads view (popup showing a message and all replies, "subscribe" to a thread) and the reply button / reply-mention stripping setting** — follows conversations in fast chat; helps mods read context — M
- **Link and image previews (hover tooltip with page title, thumbnail size setting), unshorten links, double-click to open, open in incognito** — safer clicking and richer context; needs a safe fetch proxy — M
- **Username display: colorize nicknames, bold usernames, @-mention completion with or without comma, find-all-usernames** — readability and typing speed — S
- **Nickname aliases (show a custom name for a user, regex support)** — mods/streamers recognise people by real names; stored per user — S
- **User notes (markdown notes shown in the user card)** — mods keep context about regulars and repeat offenders — S
- **Pronouns in the user card** [Twitch, alejo.io] — small inclusion/clarity feature — S
- **Timestamps (format, show seconds, per-message "show timestamp" toggle) and message length/separate-message styling** — taste and accessibility; some mods need the time stamp always — S
- **Last-message-read line color and fade of backlog history** — shows where you stopped reading after switching tabs — S
- **Emote options: animation on/off, zero-width emotes, size scaling, stack repeated emotes, per-provider enable switches (7TV/BTTV/FFZ, channel/global)** — performance on weak PCs and personal taste — S
- **Emote popup with search, categories, channel/global/subscriber tabs, and recently used** — find an emote quickly; "history" makes the common ones one click; Chatterino also has an emoji viewer with categories — M
- **Badge options (show/hide per kind: subscriber, vanity, predictions, 7TV/BTTV/FFZ/Chatterino) and a custom badge picker** — declutters the line — S
- **Bits/cheer stacking, redeemed-reward messages and a channel-point rewards view** [Twitch] — mods/streamers see redeems inline; Chatterino shows reward text with highlight options — M
- **Whispers: inline whispers or a dedicated whisper split, send via /w** [Twitch] — a lot of mod chatter happens in whispers; no equivalent on other platforms — M
- **Chatter list / viewer list popup (who is in chat) and /chatters, /mods, /vips lists** [Twitch] — find a user to moderate, see mods online — S
- **Appearance of deleted messages: strikethrough vs hidden vs "<message deleted>" with a click-to-reveal** — lets mods read what was removed — S
- **Raw IRC / raw message commands (/raw, /fakemsg for testing)** [Twitch IRC] — debugging aid for developers and bot makers — S

## Windows, themes and customization

- **Multiple windows and popouts (open a split or a tab as its own window, always on top)** — second-monitor chat while a game is fullscreen — M
- **Always-on-top and frameless/embedded window, overlay popup with click-through, scale, shadow and an "inertia" toggle** — a chat overlay over a game or stream window; Chatterino's overlay is a transparent chat that can ignore the mouse — L
- **Themes (light/dark/custom colors and a theme multiplier), window opacity and custom window frame** — personal look; a small token set mapped to CSS variables makes this M — M
- **Font settings (family, size, bold weight) and UI scale** — readability on 4K / small displays — S
- **Split header content options (viewer count, title, game, uptime) and tooltips** — see the stream state at a glance — S
- **Account switcher (several Twitch accounts, quick swap from the title bar)** — mods with a main + alt, or a bot account — M
- **Plugin system (Lua): commands, message hooks, JSON/HTTP/file APIs** — extensibility; high effort and a security surface in Electron, so look at a restricted JS/command API instead — L
- ★8 **Custom commands / aliases with parameters (`$1`, `$(1+)`, `$channel.name`, ...), optionally per channel** — mods get "/t10" or "/rules" shortcuts and streamers canned messages; Chatterino stores them as user commands that expand before sending — M
- **External tools: streamlink/player (open the stream in an external player), custom player URI, image uploader (paste an image, get a link)** [Twitch] — handy for streamers who watch in VLC/mpv or share screenshots; the uploader needs a user-supplied host — M

## Data, reliability and housekeeping

- **Logging to disk (per channel and date folders, optional only-listed-channels)** — mods keep their own record; the folder path is chosen by the user; consider privacy and a size cap — M
- **Settings backup/restore dialog and a "last run crashed" dialog** — safer upgrades; useful for beta channel testers — S
- **Launch with Windows (autorun), start minimized, remember window geometry per monitor** — daily-driver comfort — S
- **Update dialog options (nightly/beta toggle, release notes before updating)** — Agu Chat already has the channels; a "what's new" dialog is the cheap addition — S
- **Welcome / first-run dialog and a debug popup (log viewer, copy diagnostics)** — easier support for the beta — S
- **Spellcheck in the input box** — fewer typos in chat — S
- **Send-wait timer / slow-mode and timeout countdown in the input box** [Twitch] — shows when you can chat again — S
- **Emote and chat connection reload (reconnect, reload emotes by scope)** — manual recovery from a stale connection or a new 7TV set — S
