# Building OM0RX-xLog

The desktop product uses React and Tauri, the Swift `xlog-prototype-worker`, the Python contest host and the Hamlib helpers. Build on the target operating system. A plain `swift build` at the repository root builds a different, older macOS application.

## Get the source and map

Clone the public source as a Git checkout. Packaging reads the current commit and tracked file modes. For a specific release, check out its tag on a local branch named `main` in a fresh clone, for example:

```sh
git clone --branch v1.6.0 --single-branch https://github.com/oliverbross/OM0RX-xLog-Releases.git xlog-source
cd xlog-source
git switch -c main
python3 tools/fetch-dxcc-map.py
npm ci --prefix prototype
```

The map helper reads the pinned geometry from the [upstream DXCC map](https://github.com/amazingproducer/dxcc-world-map/blob/f15968402e21faa192ebe35bd91cdfcc4f0fa8db/dxcc.geojson) and checks SHA-256 `e9b086eff7fb690e9990e10855f3fb494f9d1115e4af1928f6a8b545efef4568`. The public source archive omits that file. See [the geometry provenance](Sources/XLogCore/Resources/DXCC_BOUNDARIES_SOURCE.md) and [NOTICE](NOTICE) for terms. Generated files and the fetched geometry are ignored by Git.

Use a current Rust toolchain with Cargo, Node.js 22.12 or later, Python 3.12 and Git. Install the native build prerequisites below. The npm and Cargo lockfiles fix the resolved dependencies. On Windows and Linux the worker build script verifies the exact Swift Crypto pin in `prototype/portable-swift-pins.json`. Hamlib and the downloaded Python runtime components have source and checksum pins in the staging scripts and `prototype/scripts/runtime-sources.json`. Check [release provenance](https://github.com/oliverbross/OM0RX-xLog-Releases/releases) for the tool versions recorded for an official package.

## macOS Apple Silicon

Use Xcode with Swift and the command-line developer tools, along with standard Unix build tools including `make` and Perl. Build the minimum-macOS-14 Python framework with the pinned Python, OpenSSL, XZ and CA bundle, then run the native package recipe from the repository root:

```sh
python3.12 prototype/scripts/build-python-framework-macos.py
export XLOG_PYTHON_FRAMEWORK="$PWD/.build/runtime-macos-python-3.12.16-openssl-3.6.4-arm64-min14/frameworks/Python.framework"
CODE_SIGN_IDENTITY=- bash prototype/scripts/package-macos.sh
```

The result is `prototype/src-tauri/target/release/bundle/macos/OM0RX-xLog.app`. The command above signs it ad hoc for local development. Developer ID distribution and notarization require the builder's own credentials. Local builds do not use the official signature.

## Linux x86-64

Use Ubuntu 24.04 x86-64 with the Swift 6.3.3 release toolchain, Rust, Node.js and Python 3.12. Install the native WebKit, audio, USB, SQLite, secrets and packaging dependencies:

```sh
sudo apt-get install build-essential pkg-config curl git tar bzip2 libsqlite3-dev libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libcurl4-openssl-dev libxml2-dev libicu-dev libssl-dev libudev-dev libsecret-1-dev libasound2-dev dpkg-dev
bash prototype/scripts/build-worker.sh
node prototype/scripts/stage-worker.mjs
bash prototype/scripts/stage-hamlib-linux.sh
(cd prototype && npx tauri build --no-bundle)
node prototype/scripts/distribution-metadata.mjs
bash prototype/scripts/package-linux.sh
```

The DEB and relocatable archive are under `prototype/dist-linux/<commit>/`. At runtime the Linux contest host uses Ubuntu's Python 3.12. The DEB declares the system libraries in `prototype/scripts/linux-runtime-dependencies.txt`.

## Windows x64

Use PowerShell 7 and an x64 Visual Studio developer shell with the MSVC C++ build tools, Windows SDK, CMake, Ninja, `dumpbin`, the Visual C++ redistributable and the Swift 6.3.3 x64 SDK/runtime. Install Rust's `x86_64-pc-windows-msvc` toolchain, Git, Python 3.12 (available as both `python` and `python3`), Node.js, vcpkg and WebView2. In PowerShell, set `VCPKG_ROOT` to the vcpkg checkout and run:

```powershell
& "$env:VCPKG_ROOT/vcpkg.exe" install sqlite3:x64-windows
$env:XLOG_SQLITE_PREFIX = "$env:VCPKG_ROOT/installed/x64-windows"
$env:PATH = "$env:XLOG_SQLITE_PREFIX/bin;$env:PATH"
node prototype/scripts/distribution-metadata.mjs
./prototype/scripts/package-windows.ps1
```

The script stages the pinned Hamlib and Python components, builds the worker and host, verifies DLL origins and creates the portable ZIP under `dist/prototype-windows/`. To build both NSIS scopes, pass the unpacked portable **directory** and a fresh installer output directory to `prototype/scripts/package-windows-installer.ps1 -PackageDirectory <directory> -OutputDirectory <output>`. The NSIS scratch directory must be on a local NTFS volume with enough free space for the portable payload and both installers. Use the Visual Studio redistributable from a licensed installation.

## Working on the source

Run `npm test --prefix prototype` for the included frontend tests and `npm run build --prefix prototype` to type-check, compile both frontend entries and verify the window bundles. The source snapshot includes selected synthetic Swift tests for each package target. For example, on macOS run `XLOG_PORTABLE_ONLY=1 swift test --filter IndependentRotorTests` to exercise the DCU-1 transport contracts. Use disposable directories for any runtime tests.

`SOURCE-MANIFEST.json` in the public snapshot records the canonical application commit and the content hash and Git blob of each exported input. The public repository has its own history. A local rebuild embeds the public checkout commit and uses the local toolchains and signature, so its binary digest can differ from the official release.
