# Runs one app against the fake IRC server (start it first, see README) and samples memory and CPU.
# usage: .\measure.ps1 -App egui|gpui|electron [-Seconds 180] [-NoAnim]
# PrivateWSMB matches Task Manager's "Memory" column; CPU is % of one core.
param([ValidateSet("egui", "gpui", "electron")][string]$App, [int]$Seconds = 180, [switch]$NoAnim)

$repo = Resolve-Path "$PSScriptRoot\..\.."
$env:AGU_IRC_URL = "ws://127.0.0.1:6680"
# Test runs stay on the second monitor (the first may be in use), never touch the real data folder and give
# focus back to whatever had it. screen2.ps1 holds Get-Foreground / Restore-Foreground.
$screen2 = "C:\Users\Agu\AppData\Local\Temp\claude\9b433550f8e05695\f57c89a2-8c7b-49e7-a02f-409434fcc1ae\scratchpad\screen2.ps1"
if (Test-Path $screen2) { . $screen2 }
$fg = if (Get-Command Get-Foreground -ErrorAction SilentlyContinue) { Get-Foreground } else { [IntPtr]::Zero }
$dataDir = Join-Path $env:TEMP "agu-chat-bench-data"
New-Item -ItemType Directory -Force $dataDir | Out-Null
$env:AGU_DATA_DIR = $dataDir
$env:AGU_AUTH_READONLY = "1"
$env:AGU_WIN_POS = "-1075,-466,1070,880"
$logFile = Join-Path $env:TEMP "agu-bench-$App.log"
Remove-Item $logFile -ErrorAction SilentlyContinue
if ($env:AGU_BENCH_LOG) { $env:AGU_LOG = $logFile } else { Remove-Item Env:AGU_LOG -ErrorAction SilentlyContinue }
if ($NoAnim) { $env:AGU_NO_ANIM = "1" } else { Remove-Item Env:AGU_NO_ANIM -ErrorAction SilentlyContinue }

# The Electron app gets a throwaway profile with the same two splits the native apps open.
$profileDir = Join-Path $env:TEMP "agu-chat-bench-profile"
if ($App -eq "electron") {
  New-Item -ItemType Directory -Force $profileDir | Out-Null
  # JSON.parse rejects a BOM, and Windows PowerShell's utf8 encoding writes one.
  $write = { param($file, $obj) [IO.File]::WriteAllText("$profileDir\$file", ($obj | ConvertTo-Json -Depth 8), (New-Object Text.UTF8Encoding $false)) }
  $leaf = { param($id, $ch) @{ kind = "leaf"; id = $id; sources = @(@{ platform = "twitch"; channel = $ch }); modMode = $false } }
  & $write "layout.json" @{ tabs = @(@{ id = "bench"; title = ""; root = @{ kind = "row"; id = "row"; children = @((& $leaf "a" "a_g_u__"), (& $leaf "b" "benchtwo")); sizes = @(0.5, 0.5) } }); active = "bench" }
  & $write "settings.json" @{ v = 3; timestamps = $true; fontSize = 13; animatedEmotes = -not $NoAnim }
  & $write "window.json" @{ x = -1070; y = -460; width = 1116; height = 739; maximized = $false }
}

switch ($App) {
  "egui"     { $name = "agu-chat-native"; $proc = Start-Process "$repo\native\target\release\agu-chat-native.exe" -ArgumentList "a_g_u__", "benchtwo" -PassThru }
  "gpui"     { $name = "agu-chat-gpui";   $proc = Start-Process "$repo\native\target\release\agu-chat-gpui.exe" -ArgumentList "a_g_u__", "benchtwo" -PassThru }
  "electron" { $name = "electron";        $erepo = if ($env:AGU_ELECTRON_REPO) { $env:AGU_ELECTRON_REPO } else { "$repo" }; $proc = Start-Process "$erepo\node_modules\electron\dist\electron.exe" -ArgumentList ".", "--user-data-dir=$profileDir" -WorkingDirectory $erepo -PassThru }
}
Start-Sleep -Milliseconds 800
Restore-Foreground $fg

# Only the process this script launched (and, for Electron, its children): another agent may run the same exe.
function Tree-Pids([int]$root) {
  $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
  $ids = @($root); $grew = $true
  while ($grew) { $grew = $false; foreach ($p in $all) { if (($ids -contains $p.ParentProcessId) -and -not ($ids -contains $p.ProcessId)) { $ids += $p.ProcessId; $grew = $true } } }
  $ids
}

function Snap {
  $ids = Tree-Pids $proc.Id
  $ps = @($ids | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
  $priv = (Get-CimInstance Win32_PerfFormattedData_PerfProc_Process | Where-Object { $ids -contains $_.IDProcess } | Measure-Object WorkingSetPrivate -Sum).Sum
  [pscustomobject]@{
    Procs  = $ps.Count
    WS     = [math]::Round(($ps | Measure-Object WorkingSet64 -Sum).Sum / 1MB, 1)
    PrivWS = [math]::Round($priv / 1MB, 1)
    Commit = [math]::Round(($ps | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB, 1)
    Cpu    = ($ps | Measure-Object CPU -Sum).Sum
  }
}

$rows = @(); $prevCpu = $null; $prevT = $null; $t0 = Get-Date
foreach ($mark in (@(30, 60, 120, $Seconds) | Where-Object { $_ -le $Seconds } | Sort-Object -Unique)) {
  while (((Get-Date) - $t0).TotalSeconds -lt $mark) { Start-Sleep -Milliseconds 500 }
  $s = Snap; $now = Get-Date
  $cpu = if ($null -ne $prevCpu) { [math]::Round(($s.Cpu - $prevCpu) / ($now - $prevT).TotalSeconds * 100, 1) } else { $null }
  $prevCpu = $s.Cpu; $prevT = $now
  $rows += [pscustomobject]@{ App = $App; T = $mark; Procs = $s.Procs; WorkingSetMB = $s.WS; PrivateWSMB = $s.PrivWS; CommitMB = $s.Commit; CpuPctOfOneCore = $cpu }
}
$rows | Format-Table -AutoSize
if ($env:AGU_BENCH_SHOT) { $h = (Get-Process -Id $proc.Id -ErrorAction SilentlyContinue).MainWindowHandle; if ($h -and (Get-Command Save-Window -ErrorAction SilentlyContinue)) { Save-Window $h $env:AGU_BENCH_SHOT } }
foreach ($id in (Tree-Pids $proc.Id | Sort-Object -Descending)) { Stop-Process -Id $id -Force -Confirm:$false -ErrorAction SilentlyContinue }
if (Test-Path $logFile) { Write-Host "log: $logFile" }
