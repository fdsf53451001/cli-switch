param(
    [string]$Version = "latest",
    [string]$BinDir = "$HOME\.local\bin"
)

$ErrorActionPreference = "Stop"
$repo = if ($env:CLI_SWITCH_REPO) { $env:CLI_SWITCH_REPO } else { "fdsf53451001/cli-switch" }
$asset = "cli-switch-x86_64-pc-windows-msvc.exe"
$base = "https://github.com/$repo/releases"
$url = if ($Version -eq "latest") {
    "$base/latest/download/$asset"
} else {
    "$base/download/$Version/$asset"
}

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$destination = Join-Path $BinDir "cli-switch.exe"
$temporary = "$destination.download"

Write-Host "Downloading $url"
Invoke-WebRequest -Uri $url -OutFile $temporary
Move-Item -Force $temporary $destination

Write-Host "Installed: $destination"
& $destination init

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ';') -notcontains $BinDir) {
    Write-Warning "$BinDir is not on your user PATH. Add it before opening a new terminal."
}

Write-Host "Next: cli-switch sync"
Write-Host "      cli-switch mount"
Write-Host "      cli-switch status"
