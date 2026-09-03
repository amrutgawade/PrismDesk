# Regenerates tools/gnu-shim for building the egui/eframe dashboard on the GNU
# Rust toolchain. The standalone GNU install lacks a working assembler and some
# import libs (e.g. shlwapi) that eframe/winit/parking_lot need.
#
# It gathers dlltool.exe + as.exe from the installed toolchains and generates
# libshlwapi.a. After running, put this dir FIRST on PATH before building:
#   $env:Path = "$PSScriptRoot\gnu-shim;$env:Path"
# (.cargo/config.toml already adds it to the linker -L search path.)

$ErrorActionPreference = "Stop"
$shim = Join-Path $PSScriptRoot "gnu-shim"
New-Item -ItemType Directory -Force -Path $shim | Out-Null

# dlltool: from the standalone GNU install; as: from the rustup GNU toolchain
# (the standalone one ships no assembler). Both are GNU binutils and compatible.
$dlltool = Get-ChildItem "$env:ProgramFiles\Rust*GNU*\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained\dlltool.exe" -EA SilentlyContinue | Select-Object -First 1
$asm     = Get-ChildItem "$env:USERPROFILE\.rustup\toolchains\*gnu*\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained\as.exe" -EA SilentlyContinue | Select-Object -First 1
if (-not $dlltool) { $dlltool = Get-ChildItem "$env:USERPROFILE\.rustup\toolchains\*gnu*\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained\dlltool.exe" -EA SilentlyContinue | Select-Object -First 1 }
if (-not $dlltool -or -not $asm) { throw "Need a GNU Rust toolchain with dlltool.exe and as.exe (rustup toolchain install stable-x86_64-pc-windows-gnu)" }

Copy-Item $dlltool.FullName (Join-Path $shim "dlltool.exe") -Force
Copy-Item $asm.FullName     (Join-Path $shim "as.exe")     -Force

# Generate libshlwapi.a from the checked-in shlwapi.def.
& (Join-Path $shim "dlltool.exe") -S (Join-Path $shim "as.exe") -d (Join-Path $PSScriptRoot "gnu-shim\shlwapi.def") -D shlwapi.dll -l (Join-Path $shim "libshlwapi.a")
Write-Host "gnu-shim ready: dlltool.exe, as.exe, libshlwapi.a"
