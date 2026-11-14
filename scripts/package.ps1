#Requires -Version 7
<#
.SYNOPSIS
  Builds the Atropos release artifacts into dist/: a portable zip per target, and a per-user MSI
  when WiX v4+ (`wix` .NET tool) is installed. Renders the Scoop and winget manifests with the
  real SHA-256 hashes, and writes SHA256SUMS.
.EXAMPLE
  pwsh scripts/package.ps1                                   # x64 zip (+ MSI if wix is installed)
  pwsh scripts/package.ps1 -Target x86_64-pc-windows-msvc,aarch64-pc-windows-msvc -Msi
.NOTES
  Publishing (GitHub release, winget, Scoop) is done by the maintainer; this script only builds.
#>
param(
    [string[]]$Target = @('x86_64-pc-windows-msvc'),
    [switch]$Msi,
    [switch]$NoBuild,
    [string]$Out = 'dist',
    [string]$Repo = 'https://github.com/suchetkumbar/Atropos'
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$Target = ($Target -split ',') | ForEach-Object { $_.Trim() } | Where-Object { $_ }
$version = ((Get-Content (Join-Path $root 'Cargo.toml') -Raw) -split "`n" |
    Where-Object { $_ -match '^version\s*=\s*"([^"]+)"' } | Select-Object -First 1) -replace '^version\s*=\s*"([^"]+)".*', '$1'
$version = $version.Trim()
$dist = Join-Path $root $Out
New-Item -ItemType Directory -Force $dist | Out-Null
$wix = Get-Command wix -ErrorAction SilentlyContinue
if ($Msi -and -not $wix) { throw 'MSI requested but `wix` was not found (dotnet tool install --global wix).' }
$artifacts = @()

foreach ($t in $Target) {
    $short = switch -Wildcard ($t) { 'x86_64-*' { 'x64' } 'aarch64-*' { 'arm64' } default { throw "unknown target $t" } }
    if (-not $NoBuild) {
        & cargo build --release --workspace --target $t
        if ($LASTEXITCODE) { throw "cargo build failed for $t" }
    }
    $bin = Join-Path $root "target\$t\release"
    foreach ($exe in 'Atropos.exe', 'atr.exe') {
        $f = Join-Path $bin $exe
        if (-not (Test-Path $f)) { throw "missing $f" }
        $mb = (Get-Item $f).Length / 1MB
        if ($mb -gt 15) { throw ("{0} is {1:N2} MB (budget 15 MB)" -f $exe, $mb) }
    }
    $name = "atropos-$version-windows-$short"
    $stage = Join-Path $dist "stage\$name"
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $stage | Out-Null
    Copy-Item (Join-Path $bin 'Atropos.exe'), (Join-Path $bin 'atr.exe') $stage
    Copy-Item (Join-Path $root 'LICENSE-MIT'), (Join-Path $root 'LICENSE-APACHE') $stage
    if (Test-Path (Join-Path $root 'README.md')) { Copy-Item (Join-Path $root 'README.md') $stage }
    $zip = Join-Path $dist "$name.zip"
    Remove-Item $zip -ErrorAction SilentlyContinue
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
    $artifacts += $zip

    if ($wix) {
        $msiFile = Join-Path $dist "$name.msi"
        $arch = if ($short -eq 'arm64') { 'arm64' } else { 'x64' }
        & $wix.Source build (Join-Path $root 'wix\atropos.wxs') -arch $arch `
            -d "Version=$version" -d "BinDir=$bin" -d "RootDir=$root" -d "AssetsDir=$(Join-Path $root 'assets')" `
            -o $msiFile
        if ($LASTEXITCODE) { throw "wix build failed for $t" }
        $artifacts += $msiFile
    }
}
Remove-Item -Recurse -Force (Join-Path $dist 'stage') -ErrorAction SilentlyContinue

# Hashes.
$sums = foreach ($a in $artifacts) { '{0}  {1}' -f (Get-FileHash $a -Algorithm SHA256).Hash.ToLowerInvariant(), (Split-Path $a -Leaf) }
$sums | Set-Content -Encoding utf8 (Join-Path $dist 'SHA256SUMS')
function Hash-Of($file) { (Get-FileHash (Join-Path $dist $file) -Algorithm SHA256).Hash }

# Scoop manifest (portable zip; both executables on the user's PATH via shims).
$x64zip = "atropos-$version-windows-x64.zip"
if (Test-Path (Join-Path $dist $x64zip)) {
    $scoop = [ordered]@{
        version     = $version
        description = 'Atropos: a process manager for developers. Cut the right thread.'
        homepage    = $Repo
        license     = 'MIT OR Apache-2.0'
        architecture = [ordered]@{ '64bit' = [ordered]@{ url = "$Repo/releases/download/v$version/$x64zip"; hash = (Hash-Of $x64zip).ToLowerInvariant() } }
        bin         = @('atr.exe')
        shortcuts   = @(, @('Atropos.exe', 'Atropos'))
        checkver    = 'github'
        autoupdate  = [ordered]@{ architecture = [ordered]@{ '64bit' = [ordered]@{ url = "$Repo/releases/download/v`$version/atropos-`$version-windows-x64.zip" } } }
    }
    $arm = "atropos-$version-windows-arm64.zip"
    if (Test-Path (Join-Path $dist $arm)) {
        $scoop.architecture['arm64'] = [ordered]@{ url = "$Repo/releases/download/v$version/$arm"; hash = (Hash-Of $arm).ToLowerInvariant() }
    }
    $scoop | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 (Join-Path $dist 'atropos.json')
}

# winget manifests (per-user MSI), only when the MSI was built.
$x64msi = "atropos-$version-windows-x64.msi"
if (Test-Path (Join-Path $dist $x64msi)) {
    $wg = Join-Path $dist 'winget'
    New-Item -ItemType Directory -Force $wg | Out-Null
    $id = 'Atropos.Atropos'
@"
PackageIdentifier: $id
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
"@ | Set-Content -Encoding utf8 (Join-Path $wg "$id.yaml")
    $installers = "  - Architecture: x64`n    InstallerUrl: $Repo/releases/download/v$version/$x64msi`n    InstallerSha256: $(Hash-Of $x64msi)`n"
    $armmsi = "atropos-$version-windows-arm64.msi"
    if (Test-Path (Join-Path $dist $armmsi)) {
        $installers += "  - Architecture: arm64`n    InstallerUrl: $Repo/releases/download/v$version/$armmsi`n    InstallerSha256: $(Hash-Of $armmsi)`n"
    }
@"
PackageIdentifier: $id
PackageVersion: $version
InstallerType: wix
Scope: user
InstallModes: [interactive, silent, silentWithProgress]
UpgradeBehavior: install
Commands: [atr]
Installers:
$installers
ManifestType: installer
ManifestVersion: 1.6.0
"@ | Set-Content -Encoding utf8 (Join-Path $wg "$id.installer.yaml")
@"
PackageIdentifier: $id
PackageVersion: $version
PackageLocale: en-US
Publisher: The Atropos contributors
PackageName: Atropos
License: MIT OR Apache-2.0
ShortDescription: A process manager for developers. Cut the right thread.
PackageUrl: $Repo
Tags: [process-manager, developer-tools, port, kill]
ManifestType: defaultLocale
ManifestVersion: 1.6.0
"@ | Set-Content -Encoding utf8 (Join-Path $wg "$id.locale.en-US.yaml")
}

Write-Host "Artifacts in $dist" -ForegroundColor Green
$sums | ForEach-Object { Write-Host "  $_" }
