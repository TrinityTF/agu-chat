# GPUI parity checklist (round 4A)

Electron (reference) vs the native app, same window size, same content (`layout/dev_mock.rs` = `devMock.ts`, chat
harness frames = `chat/dev/harness.tsx`). "match" = identical to within 1 px / anti-aliasing; "differs" = a known
visible difference. Electron states are reached with the dev harnesses (`chat/dev/harness.tsx` takes `set=<json>` for a
settings patch, `layout/dev/settings.html?s=<section>`) and in-page `.click()` calls over read-only CDP: no OS input.

Known capture offsets, not bugs: GPUI's window has a 1 px DWM border row at the top (the pairs crop it), and Chromium
draws text with ClearType (colour fringes) where GPUI draws grey-scale anti-aliasing.

| Screen / state | Status | Note |
| --- | --- | --- |
| Main window, tab bar, window buttons | match | line positions equal to the pixel once the DWM row is cropped |
| Split header (single source), icons, LIVE badge | match | |
| Split header, merged chips (5 platforms) | match | |
| Split menu | match | position of the demo menu is arbitrary, the real one anchors to the button |
| Channel dialog (add split) | match | last section keeps `space-y-3`'s 12 px under it (hidden submit button) |
| Channel dialog (change) | match | not paired with Electron (menu item text has an ellipsis character), layout shares the code above |
| Close-tab confirm | match | 0.6 % pixels differ |
| Chat rows: Twitch (chat, notice, raid, reply, cheer, redeem, action, deleted, first-time, mention) | match | |
| Chat rows: Kick / Rumble / YouTube / X | match | Rumble emote SVG data-URL is not decoded (real Rumble emotes are PNG); Kick moderator badge is a glyph, Electron draws its sword icon |
| YouTube Super Chat row | match | `rgba()` backgrounds are now parsed (was black) |
| Rumble rant row | differs | tier tint slightly greener than Electron |
| Pinned card, menu, "Unpin After" | match | the amount field is centred now (`TextInput::set_centered`) |
| Automod card, mod feed, suspicious label | match | |
| Search bar, "More messages" pill | match (round 2) | |
| Send box idle / typing / multiline / disabled | match (round 3A) | |
| Autocomplete (`/`, `:`, `@`) | match | |
| Emote picker | match | size, search, provider tabs; the mock bridge has no Twitch emotes, so the Twitch section is missing there |
| User card | match | pop-out is resizable now: once the user drags an edge the card fills the window (Electron's `PopoutFill`); needs a hand test |
| Mod popover, channel modes | match (round 3A) | |
| Settings: Accounts, Appearance, Highlights, Ignores, Hotkeys, Mod buttons, Updates, About | match | 1.5 to 3.5 % pixels differ (text AA); every row line sits on the same pixel |
| Settings: range slider | match | Chromium track (8 px, #efefef under the accent part) |
| Settings: select | match | `select { background: #16161d }` and the bold Chromium arrow; the open list is the OS popup in Electron and our own list here |
| Dashed borders | match | `border_dashed()` renders dashed (the old note was stale) |
| Toasts (2800 ms / 6000 ms), fade | match (timings); no fade-in in Electron either | |
| Connecting indicator | match | now an arc that spins at 1 turn/s, like `animate-spin` |
| Text input caret | match | blinks at the system caret blink time (`GetCaretBlinkTime`), solid while typing |
| Wheel scrolling | match | 100 px per notch at 3 lines (was 60) |
| Window placement | fixed | bounds on a second monitor opened on the primary one (GPUI needs the display id) |
| Hover transitions (150 ms colour fades, toggle knob slide, chevron rotate) | differs | GPUI has no CSS transitions; states switch instantly |
| Smooth scrolling | differs | Chromium animates wheel steps (~150 ms); the list jumps |
| 7TV image-textured paints and paint drop shadows | differs | not drawn (flat colour fallback) |
| Row text baseline (`TEXT_DY` = -1) | differs by <= 1 px | checked at 10, 13, 16 and 20 px: text sits 1 px higher inside its row than Chromium's at every size (icons and backgrounds are right), so the constant is size independent; left as is because a 1 px shift of every row would need the list's pixel snapping reworked |
| Native `<select>` open popup, OS colour picker | differs | deliberate (own list / palette) |

## Not verifiable without a hand

- Hover states (row toolbar, buttons), focus rings after a click, drag and drop of tabs / splits, resizing the user card
  by dragging its edge, wheel feel, IME composition, the real caret blink on a focused field.
