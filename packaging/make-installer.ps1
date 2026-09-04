# Builds the PrismDesk installer (Setup.exe): assembles the portable payload,
# then compiles the NSIS installer. Run:  powershell -File packaging\make-installer.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# 1) Build the portable payload (release exe + icon + assets + bundled adb).
& powershell -ExecutionPolicy Bypass -File "$PSScriptRoot\make-portable.ps1"
if ($LASTEXITCODE -ne 0) { throw "make-portable failed" }

# 2) Locate makensis (installed via: winget install NSIS.NSIS).
$makensis = @("C:\Program Files (x86)\NSIS\makensis.exe", "C:\Program Files\NSIS\makensis.exe") |
            Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $makensis) { throw "makensis.exe not found - install NSIS (winget install NSIS.NSIS)" }

# 3) Compile the installer. Run from packaging\ so the .nsi relative paths resolve.
Write-Host "==> Building installer with NSIS..."
Push-Location $PSScriptRoot
try {
  & $makensis "installer.nsi"
  if ($LASTEXITCODE -ne 0) { throw "makensis failed" }
} finally {
  Pop-Location
}

$setup = Join-Path $root "dist\PrismDesk-0.1.0-setup.exe"
if (Test-Path $setup) {
  Write-Host ("`nInstaller ready: {0}  ({1:N0} bytes)" -f $setup, (Get-Item $setup).Length)
} else {
  throw "Installer was not produced."
}
