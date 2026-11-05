# Benchmark: native prototype vs Electron beta under identical chat load

Both apps read the same synthetic chat from a local fake Twitch IRC server, so no other
streamer's chat is ever touched and the load is repeatable.

## 1. Fake IRC server

```powershell
cd D:\webAPP\agu-chat
node native\bench\fake-irc.mjs            # ws://127.0.0.1:6680, 20 msg/s per joined channel
# optional: $env:RATE=40; $env:SEED=7; $env:PORT=6680
```

It answers the Twitch login handshake, replies to `JOIN` with `ROOMSTATE` (room-id of `a_g_u__`, so
real 7TV/BTTV/FFZ channel emotes resolve), then streams PRIVMSGs (500 users, badges, ~40% with
emotes, some Twitch native emotes, `/me`, CJK/emoji), plus USERNOTICE subs, CLEARMSG and a CLEARCHAT
timeout every ~30 s. Same `SEED` gives the same sequence.

## 2. Native apps (two splits)

`native/` is a cargo workspace: `core` (shared networking, parsing, image decoding, chat model),
`egui` (binary `agu-chat-native`) and `gpui` (binary `agu-chat-gpui`). One shared `target/`.

```powershell
cd D:\webAPP\agu-chat\native
# one-time build: MSVC toolchain (pinned in rust-toolchain.toml) + Windows SDK
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
cargo build --release              # builds both; or: -p agu-chat-native / -p agu-chat-gpui

$env:AGU_IRC_URL = "ws://127.0.0.1:6680"
.\target\release\agu-chat-native.exe a_g_u__ benchtwo     # egui
.\target\release\agu-chat-gpui.exe   a_g_u__ benchtwo     # GPUI
# F12 toggles the stats overlay. AGU_LOG=1 writes <exe name>.log, AGU_NO_ANIM=1 freezes animations.
```

Without `AGU_IRC_URL` they connect to real Twitch (only use it on your own channel).

## 3. Electron app (two splits of the same channels)

On the `native-prototype` branch `AGU_IRC_URL` is honoured by `src/main/twitch/ircHub.js` and the
robotty backlog is skipped (`src/main/twitch/backlog.js`):

```powershell
cd D:\webAPP\agu-chat
$env:AGU_IRC_URL = "ws://127.0.0.1:6680"
npm run dev        # or the packaged beta exe launched from the same shell
```

Then open two splits for channels `a_g_u__` and `benchtwo` so both apps carry the same load.

## 4. Sum working set / private bytes for every process of an app

```powershell
function Sum-App($name) {
  $ps = Get-Process | Where-Object { $_.ProcessName -like $name }
  [pscustomobject]@{
    Procs        = $ps.Count
    WorkingSetMB = [math]::Round(($ps | Measure-Object WorkingSet64 -Sum).Sum / 1MB)
    PrivateMB    = [math]::Round(($ps | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB)
    CpuSeconds   = [math]::Round(($ps | Measure-Object CPU -Sum).Sum, 1)
  }
}
Sum-App 'agu-chat-native'      # egui
Sum-App 'agu-chat-gpui'        # GPUI
Sum-App 'Agu Chat*'            # Electron (adjust to the exe/process name; `electron` in dev)
```

For CPU%, sample `CpuSeconds` twice, N seconds apart: `(c2 - c1) / N / logicalCores * 100`.
