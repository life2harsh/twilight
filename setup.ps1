param([switch]$ClientOnly)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

function Step($text) { Write-Host "`n== $text" -ForegroundColor Cyan }
function Have($name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }
function Run {
    $exe, $rest = $args
    & $exe @rest
    if ($LASTEXITCODE) { throw "$args failed with exit code $LASTEXITCODE" }
}
function RefreshPath {
    $env:Path = @(
        [Environment]::GetEnvironmentVariable('Path', 'Machine'),
        [Environment]::GetEnvironmentVariable('Path', 'User'),
        "$HOME\.local\bin", "$HOME\.cargo\bin", "$env:ProgramFiles\CMake\bin", "$env:ProgramFiles\NASM"
    ) -join ';'
}
function Winget($id, $extra = @()) {
    Write-Host "Installing $id"
    Run winget install -e --id $id --accept-source-agreements --accept-package-agreements @extra
    RefreshPath
}
function HaveMsvc {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    (Test-Path $vswhere) -and (& $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
}

RefreshPath

Step 'Python client'
if (-not (Have uv)) {
    Invoke-RestMethod https://astral.sh/uv/install.ps1 | Invoke-Expression
    RefreshPath
}
Push-Location client
try { Run uv sync; Run uv run pytest -q } finally { Pop-Location }

if ($ClientOnly) {
    Write-Host "`nDone. Run: cd client; uv run client.py HOST_IP --pin PIN"
    exit 0
}

Step 'Rust host'
if (-not (Have winget)) { throw 'winget is missing. Install "App Installer" from the Microsoft Store.' }
if (-not (HaveMsvc)) {
    Winget Microsoft.VisualStudio.2022.BuildTools @('--override', '--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended')
}
if (-not (Have cmake)) { Winget Kitware.CMake }
if (-not (Have nasm)) { Winget NASM.NASM }
if (-not (Have cargo)) { Winget Rustlang.Rustup; Run rustup default stable }

Push-Location host
try { Run cargo build --release; Run cargo test --release -q } finally { Pop-Location }

Write-Host @'

Done.
  Host:   host\target\release\twilight-host.exe --source synthetic --pin 123456
  Client: cd client; uv run client.py 127.0.0.1 --pin 123456
Allow the host through Windows Firewall when asked, on private networks.
'@
