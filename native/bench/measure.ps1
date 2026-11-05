# Runs one app against the fake IRC server (start it first, see README) and samples memory and CPU.
# usage: .\measure.ps1 -App egui|gpui|electron [-Seconds 180] [-NoAnim]
# PrivateWSMB matches Task Manager's "Memory" column; CPU is % of one core.
param([ValidateSet("egui", "gpui", "electron")][string]$App, [int]$Seconds = 180, [switch]$NoAnim)

$repo = Resolve-Path "$PSScriptRoot\..\.."
$env:AGU_IRC_URL = "ws://127.0.0.1:6680"
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
  & $write "window.json" @{ x = 200; y = 150; width = 1116; height = 739; maximized = $false }
}

switch ($App) {
  "egui"     { $name = "agu-chat-native"; Start-Process "$repo\native\target\release\agu-chat-native.exe" -ArgumentList "a_g_u__", "benchtwo" | Out-Null }
  "gpui"     { $name = "agu-chat-gpui";   Start-Process "$repo\native\target\release\agu-chat-gpui.exe" -ArgumentList "a_g_u__", "benchtwo" | Out-Null }
  "electron" { $name = "electron";        Start-Process "$repo\node_modules\electron\dist\electron.exe" -ArgumentList ".", "--user-data-dir=$profileDir" -WorkingDirectory $repo | Out-Null }
}

function Snap {
  $ps = @(Get-Process -Name $name -ErrorAction SilentlyContinue)
  $ids = $ps.Id
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
Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process -Confirm:$false
