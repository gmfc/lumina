<#
.SYNOPSIS
  lumina installer for Windows — fetches a prebuilt lmn.exe from GitHub Releases.

.DESCRIPTION
  Run in PowerShell:

    irm https://raw.githubusercontent.com/gmfc/lumina/main/install.ps1 | iex

  Environment overrides:
    LMN_VERSION      release tag to install (default: latest), e.g. v0.1.0
    LMN_INSTALL_DIR  where to put the binary (default: %LOCALAPPDATA%\Programs\lmn)
    LMN_REPO         owner/repo to download from (default: gmfc/lumina)
#>

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Windows PowerShell 5.1 (.NET Framework) can default to TLS 1.0/1.1, which GitHub refuses.
# Force TLS 1.2 so a standalone run downloads cleanly; harmless on PowerShell 7 (.NET Core).
try {
  [Net.ServicePointManager]::SecurityProtocol =
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {}

$repo    = if ($env:LMN_REPO)    { $env:LMN_REPO }    else { 'gmfc/lumina' }
$version = if ($env:LMN_VERSION) { $env:LMN_VERSION } else { 'latest' }
$installDir = if ($env:LMN_INSTALL_DIR) {
  $env:LMN_INSTALL_DIR
} else {
  Join-Path $env:LOCALAPPDATA 'Programs\lmn'
}

# We publish a single x86_64 Windows build; it runs natively on x64 and under
# emulation on ARM64 Windows, so map every arch to that asset.
$target = 'x86_64-pc-windows-msvc'
$asset  = "lmn-$target.zip"

$base = if ($version -eq 'latest') {
  "https://github.com/$repo/releases/latest/download"
} else {
  "https://github.com/$repo/releases/download/$version"
}
$url = "$base/$asset"

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("lmn-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

try {
  $zip = Join-Path $tmp $asset
  Write-Host "downloading $asset ($version) ..."
  Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing

  # Verify the SHA-256 checksum when the sidecar file is published.
  $shaFile = "$zip.sha256"
  $haveSha = $true
  try {
    Invoke-WebRequest -Uri "$url.sha256" -OutFile $shaFile -UseBasicParsing
  } catch {
    $haveSha = $false
    Write-Warning "no published checksum for $asset - skipping verification"
  }
  if ($haveSha) {
    $expected = ((Get-Content $shaFile -Raw).Trim() -split '\s+')[0].ToLower()
    $actual   = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
    if ($expected -ne $actual) {
      throw "checksum mismatch (expected $expected, got $actual)"
    }
  }

  Expand-Archive -Path $zip -DestinationPath $tmp -Force

  # The archive holds a single versioned folder (lmn-<version>-<target>\lmn.exe);
  # locate the binary rather than assume the folder name.
  $src = Get-ChildItem -Path $tmp -Recurse -Filter 'lmn.exe' | Select-Object -First 1
  if (-not $src) { throw "could not find lmn.exe inside the downloaded archive" }

  New-Item -ItemType Directory -Force -Path $installDir | Out-Null
  $dest = Join-Path $installDir 'lmn.exe'
  # Windows won't let you overwrite a running .exe, but it will let you rename one. Move any
  # existing (possibly running) binary aside so `lmn update` works while the editor is open;
  # the leftover .old is cleaned up on the next run once it's no longer locked.
  $old = "$dest.old"
  Remove-Item $old -Force -ErrorAction SilentlyContinue
  if (Test-Path $dest) {
    try { Move-Item $dest $old -Force } catch { }
  }
  Copy-Item $src.FullName $dest -Force
  Write-Host "installed lmn -> $dest"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

# Add the install dir to the user PATH if it isn't there already.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $userPath) { $userPath = '' }
$onPath = $userPath.Split(';') | Where-Object { $_.TrimEnd('\') -ieq $installDir.TrimEnd('\') }
if (-not $onPath) {
  $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
  [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
  $env:Path = "$env:Path;$installDir"
  Write-Host ""
  Write-Host "added $installDir to your user PATH (open a new terminal to pick it up)"
}

Write-Host ""
Write-Host "done. Open the current directory with:"
Write-Host "  lmn ."
