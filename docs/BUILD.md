## 1. The one-command build

The rubric's highest-weighted criterion says *"builds in one command"*. For a judge with
Rust installed and nothing else:

```
cargo build --release
```

---

## 2. Toolchain — locked

| | |
| --- | --- |
| Toolchain | **`1.97.1-x86_64-pc-windows-gnu`** |
| `RUSTUP_HOME` | `D:\Aniket\rust\.rustup` |
| `CARGO_HOME` | `D:\Aniket\rust\.cargo` |
| `CARGO_TARGET_DIR` | `D:\Aniket\rust\tmp\target` |

**GNU, not MSVC — locked 2026-08-16, do not revisit.** MSVC needs Visual Studio Build
Tools, which needs gigabytes on C:, and every piece of verification below — the
reproducible-build hashes, the clean-envelope check, the UDP default-route probe — was
performed against GNU. Switching would invalidate the evidence for zero gain. **Including
if a linker error tempts you at hour 50.**

C: was full (0.3 GB free) at the start of preflight; the whole toolchain was relocated to
D: and the build verified to consume **0 MB on C:**. C: is now at ~49 GB free.

---

## 3. Reproducible build — +5, verified working

**It did not work out of the box.** Two clean builds with only `--remap-path-prefix` and
`CARGO_INCREMENTAL=0` produced *different* binaries:

```
build 1: 702BB466…E7272E01
build 2: 2FED7044…38C26261   ✗
```

The cause was a **PE/COFF header timestamp the MinGW linker embeds.** Adding
`-Wl,--no-insert-timestamp` fixed it:

```
build 1: E2644016AE64E78CC1CE469E4420BA30230F645C9897EF67B81289B2866EAA56
build 2: E2644016AE64E78CC1CE469E4420BA30230F645C9897EF67B81289B2866EAA56   ✓
```

Re-verified cold later the same day: two clean builds → identical `B9CE7746…FA6BB5FB`.

### The recipe

```powershell
$env:CARGO_INCREMENTAL  = "0"
$env:SOURCE_DATE_EPOCH  = "1000000000"
$env:RUSTFLAGS = "--remap-path-prefix=<abs-project-path>=. -Clink-arg=-Wl,--no-insert-timestamp"
cargo +1.97.1-x86_64-pc-windows-gnu build --release --target x86_64-pc-windows-gnu
```

**The trap:** `rust-toolchain.toml` resolves against the **working directory**, not
`--manifest-path`. Passing `--manifest-path` from elsewhere silently ignores the pin and
falls back to the MSVC host toolchain. That costs one build cycle now and an hour at hour
68. **Always use the explicit `+toolchain` and `--target` flags in the build script.**

### The envelope, stated honestly in the README

Same machine, same toolchain version, same target. `C:\MinGW\bin` is on `PATH` twice and
`gcc`/`ld` resolve there — which looked like a hidden dependency, so it was checked: the
`windows-gnu` toolchain is self-contained and does not need the separate MinGW install.
Verified by scrubbing MinGW from `PATH` and rebuilding.

---

## 4. Machine environment

Everything zql needs to test against is already installed. No preflight gap.

| Tool | Location | Used as |
| --- | --- | --- |
| **`psql` 16.2** | `D:\Aniket\rust\tmp\pg\pgsql\bin\psql.exe` *(not on PATH)* | Primary protocol oracle |
| **Node 22** | on PATH | `node-postgres` — a second, independent protocol client |
| **Python 3.11** | on PATH | `sqlite3` oracle for the SQLite reader; fixture generation |
| git 2.47.1, GitHub CLI | on PATH | submission |
| Docker, AWS CLI v2, Go, Java, MySQL, WSL, Excel 2016 | | not needed by zql |

**Two independent protocol clients matters.** `psql` and `node-postgres` share no code, so
agreement between them is real evidence rather than one implementation's quirks.

### Known gotchas on this machine

- **PowerShell `2>&1` on `cargo`** produces a spurious `NativeCommandError` and exit 1 even
  on a successful build. Cosmetic. Do not chase it at hour 40.
- **Python printing non-ASCII** to the console dies with `UnicodeEncodeError` under the
  default codepage. Set `PYTHONIOENCODING=utf-8`. The *data* was always fine — only the
  print failed.
- **`PATH` edits do not reach already-open shells.** At kickoff, open a **fresh** terminal
  first, or the first thing that happens is a phantom "Rust isn't installed" panic.

---

## 5. Networking

The dashboard binds a local HTTP port; the server binds 5432.

Verified 2026-08-16: `0.0.0.0:8080` reachable from a phone on the same LAN, HTTP 200.

**One real finding:** this machine reports **seven** non-loopback IPv4 interfaces, **six of
them `169.254.x.x` link-local junk**. A naive "enumerate interfaces, take the first
non-loopback" prints an address that leads nowhere. The fix is the UDP default-route trick
— bind `0.0.0.0:0`, `connect()` to a routable address, read `local_addr()` — which asks the
kernel which interface carries the default route. **Proven in compiled Rust, not just in
theory.** Keep a `--host` override for the no-default-route case.

---

## 6. Pre-kickoff checklist

- [x] Registration confirmed at zerodepshack.com — 2026-08-16
- [x] Toolchain installed and pinned, on D:
- [x] Reproducible build verified byte-identical, twice, including from a cold shell
- [x] MSVC-vs-GNU decided and locked
- [x] C: freed to ~49 GB
- [x] LAN reachability proven from a phone
- [x] `psql` and node-postgres available as oracles
- [x] SQLite reader spiked and passed against the Python oracle
- [ ] **Delete all four spikes** — `scratchpad/pgspike/`, `jpegspike/`, `sqlspike/`, `conspike/`
- [ ] Open a **fresh** terminal at kickoff before doing anything else
