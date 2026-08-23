# Building MGE XE

This is the source of truth for build prerequisites, targets, profiles, packaging, and
deployment. The whole product builds from one Cargo workspace. There is no Visual Studio
solution or MSBuild step; `build.rs` compiles the C++ DLL sources through the `cc` crate.

## Verification During Development

Do not build the entire product in release mode for every change. Use the smallest command
that exercises the affected area:

```powershell
# x64 Rust components
cargo check -p MGEXEgui
cargo test -p MGEXEgui
cargo check -p mgeHost64
cargo test -p mgeHost64
cargo test -p mge-config

# 32-bit DLL components; build.rs also compiles their C++ sources
cargo check -p d3d8
cargo check -p dinput8
cargo run -p config-contract-test

# generator subtree only
cargo check -p distantland -p 'distantland_*'

# everything at once
cargo check --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

For a full test sweep, exclude the four 32-bit crates. `d3d8` and `dinput8` are `cdylib`s, so
their test binary is a DLL that no test runner can execute; the two contract tests are console
harnesses driven by `cargo run`, not `#[test]` suites:

```powershell
cargo test --workspace --exclude d3d8 --exclude dinput8 `
    --exclude dl-contract-test --exclude config-contract-test
```

Documentation-only changes require no compile. Run a full build only for cross-component or
build-system changes, release validation, or when explicitly requested. Packaging, deployment,
and maximum-performance builds are never routine verification steps.

## Full-Product Commands

```powershell
cargo xtask build --release      # all four binaries plus assets into bin\MSVC-Release\
cargo xtask deploy --release     # build and install into a live Morrowind directory
cargo xtask build --max-perf     # slow maximum-performance build into bin\MSVC-MaxPerf\
cargo xtask dist                 # max-perf build packed into dist\MGE-XE-G7-v<version>.7z
cargo clean --workspace          # clean workspace packages without wiping unrelated outputs
cargo xtask compdb               # regenerate compile_commands.json for clangd/Serena
```

`cargo xtask` needs no separate installation. It is an alias in `.cargo/config.toml` for
`cargo run --package xtask --release --`; for example,
`cargo run -p xtask -- build --release` is equivalent to
`cargo xtask build --release`.

## Outputs and Targets

| Output | Package | Target | Purpose |
| --- | --- | --- | --- |
| `d3d8.dll` | `d3d8` | `i686-pc-windows-msvc` | Main injected DLL; intercepts D3D8 and contains the rendering code |
| `dinput8.dll` | `dinput8` | `i686-pc-windows-msvc` | DirectInput shim that redirects into `d3d8.dll` |
| `mgeHost64.exe` | `mgeHost64` | `x86_64-pc-windows-msvc` | Headless IPC/culling host and startup distant-land generator |
| `MGEXEgui.exe` | `MGEXEgui` | `x86_64-pc-windows-msvc` | Native Rust/egui configuration and distant-land GUI |

`d3d8/crates/dl-contract-test` is an additional i686 console harness that validates generated
distant-land output. `d3d8/crates/config-contract-test` loads the TOML defaults through the
real C++/Rust FFI path and verifies all C++ runtime bindings against `mge-config`. Packaging
and deployment are implemented by `xtask`, which replaced `RELEASE.vcxproj`.

The DLLs must stay 32-bit because Morrowind is a 32-bit process. The GUI and host helper are
64-bit. `d3d8` is a Rust `cdylib` whose Rust surface provides the `mge-config` C ABI;
`build.rs` compiles the C++ under `d3d8/cpp/` and links the public runtime exports
from `cpp/exports.def`.

## Getting the Source

```powershell
git clone https://github.com/Greatness7/MGE-XE.git
cd MGE-XE
```

There are no submodules and no sibling checkouts to arrange. Every dependency resolves from
crates.io or a public rev-pinned git repository, so a fresh clone plus the prerequisites below
is the whole setup.

## Prerequisites

- Visual Studio Build Tools. The IDE is not required. `cl.exe` compiles the C++, and `rustc`
  uses the MSVC `LIB` and `INCLUDE` environment when linking.
- DirectX SDK June 2010. Only `d3dx9.h` and `d3dx9.lib` come from this SDK; `d3d9` comes from
  the Windows SDK. `d3d8` checks `DXSDK_DIR` and then the default installation path.
- Rust nightly with the `i686-pc-windows-msvc` and `x86_64-pc-windows-msvc` targets.
  `rust-toolchain.toml` pins an exact dated nightly and makes rustup provision both.

Nightly is a permanent, deliberate requirement, not a temporary state to be fixed: the build
depends on unstable Cargo features (`per-package-target`) and unstable library features. Stable
Rust will not build this project, and patches that only remove a nightly feature gate do not
change that.

The installed product also needs the DirectX 9 June 2010 runtime for the D3DX runtime library.

## Cargo Workspace Details

All repository crates are members of the root Cargo workspace and share one `Cargo.lock` and
one `target\` directory. `[profile.*]` and `[patch.*]` settings are honored only in the root
`Cargo.toml`; copies in member manifests are ignored. Use package-scoped cleaning rather than
wiping the shared target directory.

Formatting is repo-wide: the root `rustfmt.toml` (`max_width = 125`) covers every member and
there are no per-crate copies, so `cargo fmt --all` is the whole story. The 125 came from the
`distantland` subtree, which held the value as a separate repo and is the largest body of Rust
here.

The distant-land generator lives in the `distantland\` subtree: `distantland` itself
plus the crates under `distantland\crates\`, all enumerated as workspace members by
the root manifest. It was a separate repository until 2026-08-16 and keeps its own
`ARCHITECTURE.md` and `docs\`, but not its own workspace — its shared
dependency and lint tables were lifted into the root `[workspace.dependencies]` and
`[workspace.lints]`. Every workspace member -- that subtree and MGE-XE's own crates alike --
carries `[lints] workspace = true`, so the root table is the one place clippy allowances live.
To scope a command to just that subtree:
`cargo check -p distantland -p 'distantland_*'`.

The workspace `default-members` are the x64 crates plus `xtask`, so bare `cargo build` and
`cargo check` do not attempt the 32-bit crates.

The product version lives in the root `[workspace.package]` table. `MGEXEgui`, `mgeHost64`, and
`xtask` inherit it with `version.workspace = true` and read it back through
`env!("CARGO_PKG_VERSION")`, so the GUI's version display and the `dist` archive name follow it
automatically. `d3d8` and `dinput8` stay at `0.1.0`: nothing reads their manifest version, and
bumping it changes their metadata hash and forces a full C++ rebuild.

The four 32-bit crates — `d3d8`, `dinput8`, `dl-contract-test`, `config-contract-test` — set
`forced-target = "i686-pc-windows-msvc"` in their own manifests, under Cargo's unstable
`per-package-target` feature. No `--target` flag is needed for them, and `cargo check --workspace`
works. Without it, a workspace-wide command builds them for the host, which fails outright for the
DLLs (their C++ reaches the x64 `cl.exe` and dies on `/arch:SSE2`) and, worse, silently succeeds
for the contract tests — verifying x64 layouts against a contract that only holds at 32-bit.
This feature requires nightly Cargo, which this project pins permanently anyway.

Release DLLs use the `release-dll` profile, which inherits `release` but keeps debug information
and disables stripping so `d3d8.pdb` remains available for crash diagnosis.

## Release Archive

`cargo xtask dist` produces the archive users download. It ignores the build-mode flags and
always builds `--max-perf`: there is no way to tell a slower build from a faster one once the
archive leaves the machine. It needs `7z` on `PATH`.

The four binaries go in without their PDBs, alongside the `assets/` tree and `README.md`
renamed to `MGE XE Readme.md`. Entries sit at the archive root with no wrapping folder, so it
extracts directly into a Morrowind directory.

It stages into `target\dist-staging\`, rebuilt from empty each run, rather than packing
`bin\MSVC-MaxPerf\`. That directory is maintained by `mirror_dir`, which only ever adds files,
so it accumulates assets that `assets/` has since dropped — staging fresh is what keeps them
out of a release.

## Maximum-Performance Builds

`--max-perf` selects the `max-perf` and `max-perf-dll` profiles: the release profiles plus one
codegen unit and fat LTO. It writes to `bin\MSVC-MaxPerf\`, so it does not disturb normal
release output. It is several times slower to link, is strictly opt-in,
and overrides `--release` if both flags are passed.

Cargo's LTO and codegen-unit settings affect Rust only. The C++ in `d3d8.dll` already receives
`/O2` through `cc`, so `xtask` also enables the `d3d8/max-perf` feature. That feature adds MSVC
whole-program optimization (`/GL` and `/LTCG`) plus `/Ob3`. This is what makes max-perf
meaningful for `d3d8.dll`. The nearby commented-out `/GS-` is intentionally disabled because
removing stack-cookie checks trades safety for speed.

## DLL Build Constraints

- Both DLL build scripts pass `/EHsc` explicitly. The `cc` crate does not add it; without it,
  MSVC emits warning C4530, disables unwind semantics, and drops exception-handling runtime
  imports.
- Do not fold `d3d8/cpp/main.cpp` into the `mge_d3d8` archive.
  `d3d8/build.rs` deliberately compiles it separately with
  `compile_intermediates()` and passes the object directly to the linker. An archive member
  cannot override the CRT DLL entry-point stub and fails with
  `LNK2005: _DllMain@12 already defined in msvcrt.lib(dll_dllmain_stub.obj)`, while a plain
  object can.
- The `d3d8/max-perf` feature gates `/GL` and `/LTCG`; `xtask` enables it only for
  `--max-perf`.
- `.cargo/config.toml` pins the i686 DLL linker to `link.exe`. `/GL` objects require it, and
  this keeps fresh clones from depending on per-developer linker configuration.

## Deployment

`cargo xtask deploy` resolves the Morrowind directory in this order:

1. `--morrowind-dir`
2. The `MGE_XE_MORROWIND_DIR` environment variable
3. `mge-xe-local.toml` at the repository root

The local TOML file is gitignored and uses:

```toml
morrowind_dir = "C:/path/to/Morrowind"
```

This replaces the old `MGE-XE.User.props` mechanism.
