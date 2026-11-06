# Builds the native installer:  Agu-Chat-Native-Setup-<version>-x64.exe  (+ .sha256) in -OutDir.
#   .\build-installer.ps1 -Version 0.1.0-beta.3 [-Exe ..\target\release\agu-chat-gpui.exe] [-OutDir ..\dist] [-Test]
# -Test builds an installer with its own uninstall key and shortcut name ("Agu Chat (installer test)") and a
# default folder in %TEMP%, so trying it never touches a real install. Needs NSIS (makensis): $env:NSIS_HOME,
# PATH, a choco install, or the copy electron-builder caches.
param(
  [Parameter(Mandatory)][string]$Version,
  [string]$Exe = (Join-Path $PSScriptRoot "..\target\release\agu-chat-gpui.exe"),
  [string]$OutDir = (Join-Path $PSScriptRoot "..\dist"),
  [switch]$Test
)
$ErrorActionPreference = "Stop"
if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') { throw "Version '$Version' is not x.y.z[-pre]" }
if (-not (Test-Path $Exe)) { throw "No exe at $Exe (cargo build --release -p agu-chat-gpui first)" }

function Find-Makensis {
  if ($env:NSIS_HOME -and (Test-Path "$env:NSIS_HOME\makensis.exe")) { return "$env:NSIS_HOME\makensis.exe" }
  $cmd = Get-Command makensis.exe -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  foreach ($p in "${env:ProgramFiles(x86)}\NSIS\makensis.exe", "$env:ProgramFiles\NSIS\makensis.exe") { if (Test-Path $p) { return $p } }
  $cache = Get-ChildItem "$env:LOCALAPPDATA\electron-builder\Cache" -Recurse -Filter makensis.exe -ErrorAction SilentlyContinue | Where-Object { $_.FullName -notmatch '\\(linux|mac)\\' } | Select-Object -First 1
  if ($cache) { return $cache.FullName }
  throw "makensis not found: choco install nsis, or set NSIS_HOME"
}

$ico = Join-Path $PSScriptRoot "app.ico"
if (-not (Test-Path $ico)) { & (Join-Path $PSScriptRoot "make-icon.ps1") }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$name = if ($Test) { "Agu-Chat-Native-Setup-$Version-x64-TEST.exe" } else { "Agu-Chat-Native-Setup-$Version-x64.exe" }
$out = Join-Path $OutDir $name
$nsisArgs = @("/V2", "/DVERSION=$Version", "/DEXE_SRC=$((Resolve-Path $Exe).Path)", "/DICON_SRC=$((Resolve-Path $ico).Path)", "/DOUT_FILE=$out")
if ($Test) { $nsisArgs += "/DTEST_BUILD" }
& (Find-Makensis) @nsisArgs (Join-Path $PSScriptRoot "agu-chat.nsi")
if ($LASTEXITCODE -ne 0) { throw "makensis failed ($LASTEXITCODE)" }

$hash = (Get-FileHash $out -Algorithm SHA256).Hash.ToLower()
[System.IO.File]::WriteAllText("$out.sha256", "$hash  $name`n", (New-Object System.Text.UTF8Encoding $false))
Write-Output "$out ($([math]::Round((Get-Item $out).Length / 1MB, 1)) MB) sha256 $hash"
