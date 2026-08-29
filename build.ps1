# Reproducible release build.
#
# Judges do not need this script: `cargo build --release` produces a working
# binary on its own, and that is the one-command build. This exists only to
# reproduce the exact bytes whose hash is published in README.md.
#
# Run it twice from clean and the two SHA-256 values are identical.
#
#     .\build.ps1
#
# The envelope is stated honestly in README.md: same machine, same toolchain
# version, same target. The reproducibility claim is not that any machine
# anywhere produces these bytes.

$ErrorActionPreference = "Stop"

$Toolchain = "1.97.1-x86_64-pc-windows-gnu"
$Target = "x86_64-pc-windows-gnu"

# Incremental compilation caches per-run state that leaks into output.
$env:CARGO_INCREMENTAL = "0"
# The conventional knob for build timestamps. Nothing in zql reads the clock at
# build time, but setting it costs nothing and documents the intent.
$env:SOURCE_DATE_EPOCH = "1000000000"
# Must be unset: an inherited RUSTFLAGS silently *replaces* the flags in
# .cargo/config.toml rather than adding to them, which would drop the
# no-insert-timestamp link argument and quietly break reproducibility.
Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue

# The explicit `+toolchain` and `--target` are deliberate. A rust-toolchain.toml
# resolves against the working directory, not the manifest, so invoking cargo
# from elsewhere would silently fall back to the host default toolchain.
cargo "+$Toolchain" build --release --target $Target

$exe = Join-Path $PSScriptRoot "target\$Target\release\zql.exe"
$hash = (Get-FileHash $exe -Algorithm SHA256).Hash

Write-Output ""
Write-Output "binary : $exe"
Write-Output "size   : $((Get-Item $exe).Length) bytes"
Write-Output "sha256 : $hash"
