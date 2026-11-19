$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$env:XLOG_PORTABLE_ONLY = '1'
$scratch = Join-Path $repoRoot '.build/prototype-worker'
$arguments = @('--force-resolved-versions', '--package-path', $repoRoot, '--scratch-path', $scratch, '-c', 'release')
$pinHelper = Join-Path $PSScriptRoot 'portable-swift-lock.py'
# vcpkg install sqlite3:x64-windows; set XLOG_SQLITE_PREFIX to installed/x64-windows.
if ($env:XLOG_SQLITE_PREFIX) {
    $arguments += @('-Xcc', "-I$env:XLOG_SQLITE_PREFIX/include", '-Xlinker', "/LIBPATH:$env:XLOG_SQLITE_PREFIX/lib")
}
$zlibPrefix = if ($env:XLOG_ZLIB_PREFIX) { $env:XLOG_ZLIB_PREFIX } else { $env:XLOG_SQLITE_PREFIX }
if (!$zlibPrefix -or !(Test-Path (Join-Path $zlibPrefix 'include/zlib.h'))) {
    throw 'Install zlib:x64-windows-static-md via vcpkg and set XLOG_ZLIB_PREFIX to its installation prefix.'
}
if (!(Test-Path (Join-Path $zlibPrefix 'lib/libcurl.lib'))) {
    throw 'Install curl[non-http,ssl,sspi]:x64-windows-static-md alongside zlib in XLOG_ZLIB_PREFIX.'
}
$arguments += @('-Xcc', "-I$zlibPrefix/include", '-Xlinker', "/LIBPATH:$zlibPrefix/lib")
& python3 $pinHelper run --root $repoRoot -- swift build @arguments --product xlog-prototype-worker
if ($LASTEXITCODE -ne 0) { throw 'Worker build failed.' }
$binaryDir = (& python3 $pinHelper run --root $repoRoot -- swift build @arguments --show-bin-path | Select-Object -Last 1).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Worker path lookup failed.' }
if ($env:XLOG_SQLITE_PREFIX) {
    $sqliteDLL = Join-Path $env:XLOG_SQLITE_PREFIX 'bin/sqlite3.dll'
    if (Test-Path $sqliteDLL) { Copy-Item $sqliteDLL $binaryDir -Force }
}
& (Join-Path $binaryDir 'xlog-prototype-worker.exe') --self-test
if ($LASTEXITCODE -ne 0) { throw 'Worker self-test failed.' }
Write-Host "Worker built: $binaryDir/xlog-prototype-worker.exe"
