#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PackageDirectory,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$NativeBuildDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-installer-policy.ps1')
if (-not $IsWindows) { throw 'Build the installer on native Windows.' }
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$PackageDirectory = (Resolve-Path $PackageDirectory).Path
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if ($OutputDirectory.TrimEnd('\').Equals($PackageDirectory.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase) -or
    $OutputDirectory.StartsWith($PackageDirectory.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Installer output must be outside the audited portable payload.'
}
if ((Test-Path -LiteralPath $OutputDirectory) -and @(Get-ChildItem -LiteralPath $OutputDirectory -Force).Count) {
    throw 'Installer output must be new or empty; preserve previous candidate evidence.'
}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null

function Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE." }
}
function Hash([string]$File) { (Get-FileHash -LiteralPath $File -Algorithm SHA256).Hash.ToLowerInvariant() }

$version = (Get-Content (Join-Path $repo 'prototype/package.json') -Raw | ConvertFrom-Json).version
$provenance = Get-Content (Join-Path $PackageDirectory 'provenance.json') -Raw | ConvertFrom-Json
$commit = (& git -C $repo rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $provenance.commit -ne $commit) { throw 'Portable payload source differs from checkout.' }
$sourceStatus = @(& git -C $repo status --porcelain --untracked-files=normal)
if ($LASTEXITCODE -ne 0 -or $sourceStatus.Count) { throw 'Commit source changes before packaging; installer provenance requires a clean canonical checkout.' }
$manifest = Get-Content (Join-Path $PackageDirectory 'payload-manifest.json') -Raw | ConvertFrom-Json -Depth 30
$payloadManifestHash = Hash (Join-Path $PackageDirectory 'payload-manifest.json')
if ($provenance.schema -ne 1 -or $manifest.schema -ne 1 -or $provenance.version -ne $version) {
    throw 'Portable payload schema/version differs from the installer candidate.'
}
$payloadEntries = @((Get-Item -LiteralPath $PackageDirectory)) + @(Get-ChildItem -LiteralPath $PackageDirectory -Recurse -Force)
if (@($payloadEntries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count -gt 0) {
    throw 'Portable payload cannot contain reparse points.'
}
$resources = @{}
$expected = @{}
foreach ($entry in $manifest.files) {
    $relative = [string]$entry.path
    if ([string]::IsNullOrWhiteSpace($relative) -or [IO.Path]::IsPathRooted($relative) -or
        $relative.Contains(':') -or ($relative -split '[/\\]') -contains '..') { throw 'Unsafe payload manifest path.' }
    $file = [IO.Path]::GetFullPath((Join-Path $PackageDirectory $relative))
    $canonical = [IO.Path]::GetRelativePath($PackageDirectory, $file).Replace('\', '/')
    if ($relative -cne $canonical -or $relative -ieq 'payload-manifest.json' -or $expected.ContainsKey($canonical)) {
        throw "Noncanonical or duplicate payload manifest path: $relative"
    }
    $expected[$canonical] = $true
    if (-not $file.StartsWith($PackageDirectory.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase) -or
        -not (Test-Path -LiteralPath $file -PathType Leaf) -or (Hash $file) -ne $entry.sha256 -or
        (Get-Item -LiteralPath $file).Length -ne $entry.bytes) {
        throw "Payload hash/path mismatch: $relative"
    }
    if ($relative -ne 'xlog-prototype.exe') { $resources[$file.Replace('\', '/')] = $relative }
}
$resources[(Join-Path $PackageDirectory 'payload-manifest.json').Replace('\', '/')] = 'payload-manifest.json'
$actual = @(Get-ChildItem -LiteralPath $PackageDirectory -File -Recurse -Force | Where-Object {
    [IO.Path]::GetRelativePath($PackageDirectory, $_.FullName).Replace('\', '/') -ine 'payload-manifest.json'
})
if ($actual.Count -ne $expected.Count -or -not $expected.ContainsKey('xlog-prototype.exe')) { throw 'Portable payload inventory differs from its manifest.' }
foreach ($file in $actual) {
    $relative = [IO.Path]::GetRelativePath($PackageDirectory, $file.FullName).Replace('\', '/')
    if (-not $expected.ContainsKey($relative)) { throw "Unmanifested portable payload file: $relative" }
}
$privacyReport = Join-Path $OutputDirectory "OM0RX-xLog-$version-windows-payload-privacy.json"
Checked python @((Join-Path $PSScriptRoot 'verify-release-payload.py'), $PackageDirectory, '--report', $privacyReport)

$compiledRelease = Join-Path $repo 'prototype/src-tauri/target/release'
if ((Hash (Join-Path $compiledRelease 'xlog-prototype.exe')) -ne (Hash (Join-Path $PackageDirectory 'xlog-prototype.exe'))) {
    throw 'NSIS main executable differs from the audited portable payload.'
}
# NSIS can return success with a truncated archive when its scratch/output is
# on a shared WebDAV filesystem. Keep source canonical and use Cargo's supported
# target-directory override only for bundling the already audited executable.
if (-not $NativeBuildDirectory) {
    $NativeBuildDirectory = Join-Path ([IO.Path]::GetTempPath()) ("xlog-nsis-$version-" + [Guid]::NewGuid().ToString('N'))
}
if ($NativeBuildDirectory -notmatch '^[A-Za-z]:[\\/]') { throw 'NSIS build directory must be an absolute local NTFS path.' }
$NativeBuildDirectory = [IO.Path]::GetFullPath($NativeBuildDirectory)
if ($NativeBuildDirectory.Equals($PackageDirectory.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase) -or
    $NativeBuildDirectory.StartsWith($PackageDirectory.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'NSIS scratch/output must be outside the audited portable payload.'
}
$drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($NativeBuildDirectory))
if ($drive.DriveType -ne [IO.DriveType]::Fixed -or $drive.DriveFormat -ne 'NTFS') { throw 'NSIS scratch/output requires a fixed NTFS volume.' }
for ($ancestor = $NativeBuildDirectory; $ancestor; $ancestor = [IO.Path]::GetDirectoryName($ancestor.TrimEnd('\'))) {
    if ((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'NSIS build directory must not traverse a reparse point.'
    }
}
if (Test-Path -LiteralPath $NativeBuildDirectory) { throw 'NSIS build directory must be new; preserve previous build evidence.' }
$release = Join-Path $NativeBuildDirectory 'release'
New-Item -ItemType Directory -Path $release | Out-Null
$auditedMain = Join-Path $PackageDirectory 'xlog-prototype.exe'
Copy-Item -LiteralPath $auditedMain -Destination (Join-Path $release 'xlog-prototype.exe')
if ((Hash (Join-Path $release 'xlog-prototype.exe')) -ne (Hash $auditedMain)) { throw 'Native NSIS executable staging differs from audited payload.' }
# Reuse the existing official local tool cache, without downloading/reinstalling
# a compiler or changing the source checkout's build outputs.
$toolCache = Join-Path $repo 'prototype/src-tauri/target/.tauri'
if (Test-Path -LiteralPath $toolCache -PathType Container) {
    $nativeTools = Join-Path $NativeBuildDirectory '.tauri'
    Copy-Item -LiteralPath $toolCache -Destination $nativeTools -Recurse
    foreach ($file in Get-ChildItem -LiteralPath $toolCache -File -Recurse) {
        $relative = [IO.Path]::GetRelativePath($toolCache, $file.FullName)
        if ((Hash (Join-Path $nativeTools $relative)) -ne (Hash $file.FullName)) { throw 'Native NSIS tool cache copy differs.' }
    }
}
Push-Location (Join-Path $repo 'prototype')
try {
    $bundlerVersion = (& npx.cmd tauri --version) -join ' '
    if ($LASTEXITCODE -ne 0) { throw 'Cannot record the Tauri bundler version.' }
} finally { Pop-Location }
$previousCargoTarget = [Environment]::GetEnvironmentVariable('CARGO_TARGET_DIR', 'Process')
$env:CARGO_TARGET_DIR = $NativeBuildDirectory
try {
foreach ($scope in @('currentUser', 'perMachine')) {
    $policy = Get-XLogInstallerScope $scope
    $destination = Join-Path $OutputDirectory "OM0RX-xLog-$version-windows-x86_64-$($policy.artifactLabel)-setup.exe"
    if (Test-Path -LiteralPath $destination) { throw 'Versioned installer output already exists.' }
    $configPath = Join-Path $OutputDirectory "nsis-$($policy.artifactLabel)-audited-payload.config.json"
    $configuration = New-XLogInstallerConfiguration -Scope $scope -Resources $resources
    $configuration | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $configPath -Encoding utf8NoBOM
    $previousInstallers = @{}
    Get-ChildItem (Join-Path $release 'bundle/nsis') -Filter '*-setup.exe' -File -ErrorAction SilentlyContinue |
        ForEach-Object { $previousInstallers[$_.FullName] = Hash $_.FullName }
    $started = [DateTime]::UtcNow.AddSeconds(-2)
    Push-Location (Join-Path $repo 'prototype')
    try { Checked npx.cmd @('tauri', 'bundle', '--bundles', 'nsis', '--config', $configPath, '--no-binary-patching', '--no-sign', '--ci') }
    finally { Pop-Location }
    if ((Hash (Join-Path $release 'xlog-prototype.exe')) -ne (Hash (Join-Path $PackageDirectory 'xlog-prototype.exe'))) {
        throw 'Bundling changed the audited main executable.'
    }
    foreach ($entry in $manifest.files) {
        if ((Hash (Join-Path $PackageDirectory $entry.path)) -ne $entry.sha256) { throw 'Portable payload changed during installer bundling.' }
    }
    if ((Hash (Join-Path $PackageDirectory 'payload-manifest.json')) -ne $payloadManifestHash) {
        throw 'Portable payload manifest changed during installer bundling.'
    }
    $installers = @(Get-ChildItem (Join-Path $release 'bundle/nsis') -Filter '*-setup.exe' -File |
        Where-Object {
            $_.Name.Contains($version) -and $_.LastWriteTimeUtc -ge $started -and
            (-not $previousInstallers.ContainsKey($_.FullName) -or (Hash $_.FullName) -ne $previousInstallers[$_.FullName])
        })
    if ($installers.Count -ne 1) { throw 'Expected exactly one freshly generated versioned NSIS installer.' }
    Copy-Item -LiteralPath $installers[0].FullName -Destination $destination
    if ((Get-Item -LiteralPath $destination).Length -ne $installers[0].Length -or
        (Hash $destination) -ne (Hash $installers[0].FullName)) { throw 'Delivered installer differs from native output.' }
    "$(Hash $destination)  $([IO.Path]::GetFileName($destination))" |
        Set-Content -LiteralPath "$destination.sha256" -Encoding utf8NoBOM
    @{
        schema = 2; version = $version; sourceCommit = $commit; platform = 'windows-x86_64'
        installer = [IO.Path]::GetFileName($destination); sha256 = Hash $destination
        installerBytes = (Get-Item -LiteralPath $destination).Length
        nativeBuildVolume = 'fixed NTFS'
        mainExecutableSHA256 = Hash (Join-Path $PackageDirectory 'xlog-prototype.exe')
        payloadManifestSHA256 = $payloadManifestHash
        privacyReport = [IO.Path]::GetFileName($privacyReport)
        installation = $policy
        configurationSHA256 = Hash $configPath
        bundlerVersion = $bundlerVersion
        scopeChange = 'No automatic profile migration; uninstall the previous scope with Delete app data unchecked, then install the new scope as documented'
        webview2 = 'Microsoft Evergreen bootstrapper embedded; network access is needed if WebView2 is absent; no Swift/Node/Rust installation required'
        signing = 'Unsigned Windows installer; checksums are integrity evidence, not an authenticity signature'
    } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath "$destination.provenance.json" -Encoding utf8NoBOM
    Write-Host "Windows EXE installer: $destination"
}
} finally {
    if ($null -eq $previousCargoTarget) { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue }
    else { $env:CARGO_TARGET_DIR = $previousCargoTarget }
}
