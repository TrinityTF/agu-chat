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

## 4. Measure

`measure.ps1` launches one app against the running fake server (Electron gets a throwaway profile in
`%TEMP%\agu-chat-bench-profile` with the same two splits), samples it at 30/60/120/N s and closes it:

```powershell
cd D:\webAPP\agu-chat
npm run build:renderer                     # once, for the Electron run
.\native\bench\measure.ps1 -App egui       # or gpui / electron; -Seconds 180, -NoAnim
```

`PrivateWSMB` is Task Manager's "Memory" column; CPU is % of one core.

## Results (2026-11-05, same PC, two splits, window ~1100x700, 3 min)

| Load | App | Memory (private WS) | CPU (one core) |
|---|---|---|---|
| Busy: 20 msg/s per split, animated emotes | Electron | 818 MB, still growing | 154% |
| | egui | 114 MB | 11% |
| | GPUI | 87 MB | 13% |
| Quiet: 0.5 msg/s per split, animated emotes | Electron | 226 MB | 28% |
| | egui | 82 MB | 9% |
| | GPUI | 58 MB | 13% |
| Quiet, animations frozen (`-NoAnim`) | egui | 81 MB | ~1% |
| | GPUI | 52 MB | ~1.5% |

Exe size: egui 8.9 MB, GPUI 11 MB (Electron installer ~120 MB). GPU driver memory is not fully counted
for the native apps; Electron's figure includes its GPU process.

## 5. Raw sampling


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
