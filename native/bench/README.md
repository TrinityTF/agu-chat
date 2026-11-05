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

## 2. Native app (two splits)

```powershell
cd D:\webAPP\agu-chat\native
# one-time build. This machine has no `as`/Windows SDK, so build tools/dlltool-shim first and put its dlltool.exe on PATH
$env:Path = "<dir containing the built dlltool.exe>;$env:USERPROFILE\.cargo\bin;$env:Path"
cargo build --release

$env:AGU_IRC_URL = "ws://127.0.0.1:6680"
.\target\release\agu-chat-native.exe a_g_u__ benchtwo
# F12 toggles the stats overlay. AGU_LOG=1 writes agu-chat-native.log, AGU_NO_ANIM=1 freezes animations.
```

Without `AGU_IRC_URL` it connects to real Twitch (only use it on your own channel).

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
Sum-App 'agu-chat-native'      # native
Sum-App 'Agu Chat*'            # Electron (adjust to the exe/process name; `electron` in dev)
```

For CPU%, sample `CpuSeconds` twice, N seconds apart: `(c2 - c1) / N / logicalCores * 100`.
