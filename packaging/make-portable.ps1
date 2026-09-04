# Builds the portable PrismDesk package (no installer): a self-contained folder
# + zip that runs from anywhere. Run:  powershell -File packaging\make-portable.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$env:Path = "$root\tools\gnu-shim;$env:Path"   # GNU toolchain shim (see .cargo/config.toml)

Write-Host "==> Building release binary..."
cargo build -p pd-engine --bin pd-engine --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# Version from the crate manifest.
$ver = (Select-String -Path "crates\pd-engine\Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' |
        Select-Object -First 1).Matches.Groups[1].Value

# Embed the app icon + version metadata into the exe (Explorer file icon).
# The GNU toolchain has no resource compiler, so patch the built exe with rcedit.
$rcedit = Join-Path $root "tools\bin\rcedit.exe"
if (-not (Test-Path $rcedit)) {
  Write-Host "==> Downloading rcedit..."
  New-Item -ItemType Directory -Force -Path (Split-Path $rcedit) | Out-Null
  Invoke-WebRequest "https://github.com/electron/rcedit/releases/download/v2.0.0/rcedit-x64.exe" -OutFile $rcedit
}
Write-Host "==> Embedding icon + version metadata..."
& $rcedit "$root\target\release\pd-engine.exe" `
  --set-icon "$root\crates\pd-engine\assets\icon\prismdesk.ico" `
  --set-version-string "ProductName" "PrismDesk" `
  --set-version-string "FileDescription" "PrismDesk - Android screen mirroring & control" `
  --set-version-string "CompanyName" "Amrut Gawade" `
  --set-version-string "LegalCopyright" "(c) 2026 Amrut Gawade" `
  --set-file-version "$ver.0" --set-product-version "$ver.0"

$dist = Join-Path $root "dist"
$app  = Join-Path $dist "PrismDesk"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force -Path (Join-Path $app "platform-tools") | Out-Null

Write-Host "==> Assembling $app ..."
Copy-Item "$root\target\release\pd-engine.exe" (Join-Path $app "PrismDesk.exe")
Copy-Item "$root\assets\server\scrcpy-server-v3.3.1.jar" $app
Copy-Item "$root\crates\pd-engine\assets\icon\prismdesk.ico" $app

# Bundle adb so the app is zero-setup (found via exe\platform-tools\adb.exe).
$pt = "C:\platform-tools"
if (Test-Path (Join-Path $pt "adb.exe")) {
  foreach ($f in "adb.exe","AdbWinApi.dll","AdbWinUsbApi.dll") {
    $src = Join-Path $pt $f
    if (Test-Path $src) { Copy-Item $src (Join-Path $app "platform-tools") }
  }
} else {
  Write-Host "   WARNING: $pt\adb.exe not found - adb NOT bundled (users must have adb on PATH)."
}

$readme = @"
PrismDesk $ver
Low-latency Android screen mirroring & control for Windows.

HOW TO RUN
  Double-click PrismDesk.exe. Connect your Android phone via USB with USB
  debugging enabled, then click "Start Mirror".

REQUIREMENTS
  - Windows 10/11 (64-bit)
  - An NVIDIA GPU (GTX 1650 or newer recommended) for hardware decode
  - adb is bundled in platform-tools\ - nothing else to install

IN THE MIRROR WINDOW
  Click / drag = tap & swipe    Wheel = scroll    Right-click = Back
  Type to send text.  F11 fullscreen.  Ctrl+M mute, Ctrl+S screenshot,
  Ctrl+R record, Ctrl+V paste.

Screenshots -> Pictures\PrismDesk,  Recordings -> Videos\PrismDesk.

Designed & built by Amrut Gawade - https://amrut.is-a.dev
Bundles scrcpy (Apache-2.0), Geist font (OFL-1.1), Lucide icons (ISC),
and Android platform-tools/adb. Not affiliated with Google or Genymobile.
"@
$readme | Set-Content -Encoding UTF8 (Join-Path $app "README.txt")

$thirdparty = @"
PrismDesk bundles the following third-party components:

  scrcpy server (Genymobile)      Apache-2.0
  Android platform-tools / adb    Android SDK terms (Google)
  Geist & Geist Mono fonts        SIL Open Font License 1.1
  Lucide icons                    ISC License
  egui / eframe, windows-rs       MIT / Apache-2.0
  cpal, mp4-rust, clipboard-win   MIT / Apache-2.0

Full license texts are available from each project's repository.
PrismDesk is not affiliated with Google or Genymobile.
"@
$thirdparty | Set-Content -Encoding UTF8 (Join-Path $app "THIRD-PARTY.txt")

$zip = Join-Path $dist "PrismDesk-$ver-portable-win64.zip"
Write-Host "==> Zipping -> $zip"
Compress-Archive -Path $app -DestinationPath $zip -Force

Write-Host ""
Write-Host "Portable package ready:"
Get-ChildItem -Recurse $app | ForEach-Object {
  "   " + $_.FullName.Substring($app.Length + 1) + "  (" + $_.Length + " bytes)"
}
Write-Host ""
Write-Host ("ZIP: {0}  ({1:N0} bytes)" -f $zip, (Get-Item $zip).Length)
