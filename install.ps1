<#
.SYNOPSIS
  gafos installer for Windows PowerShell.

.DESCRIPTION
  Downloads the latest (or pinned) release binary and installs it.

  Usage:
    irm https://raw.githubusercontent.com/strowk/gafos/main/install.ps1 | iex

  Environment variables:
    GAFOS_VERSION      release tag to install (default: latest)
    GAFOS_INSTALL_DIR  install directory (default: %LOCALAPPDATA%\gafos\bin)
    GITHUB_TOKEN / GH_TOKEN  token with 'repo' scope (required while the repo is private)
#>
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocol]::Tls12

$repo = 'strowk/gafos'
$api = "https://api.github.com/repos/$repo"
$installDir = if ($env:GAFOS_INSTALL_DIR) { $env:GAFOS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'gafos\bin' }
$token = if ($env:GITHUB_TOKEN) { $env:GITHUB_TOKEN } elseif ($env:GH_TOKEN) { $env:GH_TOKEN } else { $null }

$arch = $env:PROCESSOR_ARCHITECTURE
switch ($arch) {
  'AMD64' { $target = 'x86_64-pc-windows-msvc' }
  default { throw "gafos install: unsupported architecture '$arch' (only x86_64 is published)" }
}
$asset = "gafos-$target.exe"

$headers = @{ Accept = 'application/vnd.github+json' }
if ($token) { $headers['Authorization'] = "Bearer $token" }

$releaseUrl = if ($env:GAFOS_VERSION) { "$api/releases/tags/$($env:GAFOS_VERSION)" } else { "$api/releases/latest" }

try {
  $release = Invoke-RestMethod -Uri $releaseUrl -Headers $headers
}
catch {
  throw "gafos install: could not fetch release metadata (private repo? set GITHUB_TOKEN with 'repo' scope). $_"
}

$assetObj = $release.assets | Where-Object { $_.name -eq $asset } | Select-Object -First 1
if (-not $assetObj) { throw "gafos install: release $($release.tag_name) has no asset named $asset" }

Write-Host "Installing gafos $($release.tag_name) ($target) to $installDir"

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$dest = Join-Path $installDir 'gafos.exe'

# Asset API URL + octet-stream works for private and public repos alike.
$dlHeaders = @{ Accept = 'application/octet-stream' }
if ($token) { $dlHeaders['Authorization'] = "Bearer $token" }
Invoke-WebRequest -Uri $assetObj.url -Headers $dlHeaders -OutFile $dest

Write-Host "Installed: $dest"

# Add the install dir to the user PATH if it is not already there.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not ($userPath -split ';' | Where-Object { $_ -eq $installDir })) {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$installDir", 'User')
  $env:Path = "$env:Path;$installDir"
  Write-Host "Added $installDir to your user PATH (restart your shell to pick it up)."
}

Write-Host "Run 'gafos --spec <openapi.yaml> --route <httproute.yaml>' to generate your HTTPRoute rules."
