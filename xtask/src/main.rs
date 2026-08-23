use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args.first().map(|s| s.as_str()).unwrap_or("help");

    // --max-perf wins if both are given; it is the strictly stronger request.
    let mode = if args.iter().any(|a| a == "--max-perf") {
        BuildMode::MaxPerf
    } else if args.iter().any(|a| a == "--release") {
        BuildMode::Release
    } else {
        BuildMode::Dev
    };
    let morrowind_dir = args
        .iter()
        .position(|a| a == "--morrowind-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let root = workspace_root();

    match command {
        "build" => cmd_build(&root, mode),
        "dist" => cmd_dist(&root),
        "deploy" => cmd_deploy(&root, mode, morrowind_dir),
        "compdb" => cmd_compdb(&root),
        _ => print_usage(),
    }
}

fn print_usage() {
    eprintln!(concat!(
        "Usage: cargo xtask <command> [--release | --max-perf] [--morrowind-dir <path>]\n\n",
        "Commands:\n",
        "  build     Build all artifacts and mirror assets/ into bin/MSVC-<config>/\n",
        "  dist      Build --max-perf and pack a release archive into dist/\n",
        "  deploy    Copy binaries, PDBs, and assets into the Morrowind directory\n",
        "  compdb    Generate compile_commands.json for clangd/Serena\n\n",
        "Formatting and linting are plain cargo; the 32-bit crates carry their own\n",
        "forced-target, so no wrapper is needed:\n",
        "  cargo fmt --all\n",
        "  cargo clippy --workspace -- -D warnings\n\n",
        "Build modes:\n",
        "  (default)   dev profile\n",
        "  --release   release / release-dll profiles\n",
        "  --max-perf  max-perf / max-perf-dll profiles (codegen-units=1 + fat LTO; slow)\n",
        "`dist` ignores these and always builds --max-perf."
    ));
}

#[derive(Clone, Copy)]
enum BuildMode {
    Dev,
    Release,
    MaxPerf,
}

impl BuildMode {
    /// Cargo profile for the 32-bit DLLs, which keep their PDBs.
    fn dll_profile(self) -> &'static str {
        match self {
            BuildMode::Dev => "dev",
            BuildMode::Release => "release-dll",
            BuildMode::MaxPerf => "max-perf-dll",
        }
    }

    /// Cargo profile for the x64 executables.
    fn exe_profile(self) -> &'static str {
        match self {
            BuildMode::Dev => "dev",
            BuildMode::Release => "release",
            BuildMode::MaxPerf => "max-perf",
        }
    }

    /// Name used for the `bin\MSVC-*` output directory.
    fn config_name(self) -> &'static str {
        match self {
            BuildMode::Dev => "Debug",
            BuildMode::Release => "Release",
            BuildMode::MaxPerf => "MaxPerf",
        }
    }
}

/// Cargo writes `dev`-profile output to `target/<triple>/debug`; every other
/// profile's directory matches its name.
fn target_subdir(profile: &str) -> &str {
    if profile == "dev" { "debug" } else { profile }
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    manifest.parent().unwrap().to_path_buf()
}

const WIN32_TARGET: &str = "i686-pc-windows-msvc";
const WINDOWS_HOST: &str = "x86_64-pc-windows-msvc";

fn cmd_compdb(root: &Path) {
    let d3d8_cpp = root.join("d3d8/cpp");
    let dinput8_cpp = root.join("dinput8/cpp");
    let dxsdk = resolve_dxsdk();
    let analysis_out = root.join("target/compdb");
    fs::create_dir_all(&analysis_out).unwrap_or_else(|e| {
        panic!(
            "failed to create compilation-database scratch directory {}: {e}",
            analysis_out.display()
        )
    });

    let d3d8_build = d3d8_analysis_build(&d3d8_cpp, &dxsdk, &analysis_out);
    let dinput8_build = dinput8_analysis_build(&analysis_out);

    let mut sources = Vec::new();
    collect_cpp_files(&d3d8_cpp, &mut sources);
    collect_cpp_files(&dinput8_cpp, &mut sources);
    sources.sort();

    let mut entries = Vec::with_capacity(sources.len());
    for source in sources {
        let build = if source.starts_with(&d3d8_cpp) {
            &d3d8_build
        } else {
            &dinput8_build
        };
        entries.push(serde_json::json!({
            "directory": root.to_string_lossy(),
            "file": source.to_string_lossy(),
            "arguments": analysis_arguments(build, &source),
        }));
    }

    let output = root.join("compile_commands.json");
    let json = serde_json::to_string_pretty(&entries).expect("compilation-database entries should always serialize");
    fs::write(&output, format!("{json}\n")).unwrap_or_else(|e| panic!("failed to write {}: {e}", output.display()));
    println!("Generated {} compilation commands at {}", entries.len(), output.display());
}

fn analysis_build(analysis_out: &Path) -> cc::Build {
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .target(WIN32_TARGET)
        .host(WINDOWS_HOST)
        .out_dir(analysis_out)
        .opt_level(0)
        .debug(true)
        .cargo_metadata(false);
    build
}

fn d3d8_analysis_build(cpp_dir: &Path, dxsdk: &Path, analysis_out: &Path) -> cc::Build {
    let mut build = analysis_build(analysis_out);
    build
        .flag("/EHsc")
        .flag("/arch:SSE2")
        .flag("/fp:fast")
        .flag("/GR-")
        .flag("/Z7")
        .define("WIN32", None)
        .define("NOMINMAX", None)
        .define("_WINDOWS", None)
        .define("_DEBUG", None)
        .include(cpp_dir)
        .include(dxsdk.join("include"));
    build
}

fn dinput8_analysis_build(analysis_out: &Path) -> cc::Build {
    let mut build = analysis_build(analysis_out);
    build
        .flag("/EHsc")
        .flag("/GR-")
        .define("WIN32", None)
        .define("DINPUT8_EXPORTS", None)
        .define("_WINDOWS", None)
        .define("_USRDLL", None)
        .define("_DEBUG", None);
    build
}

fn analysis_arguments(build: &cc::Build, source: &Path) -> Vec<String> {
    let tool = build.get_compiler();
    let mut arguments = Vec::with_capacity(tool.args().len() + 4);
    arguments.push(tool.path().to_string_lossy().into_owned());
    arguments.extend(tool.args().iter().map(|arg| arg.to_string_lossy().into_owned()));

    // clangd does not execute this command, but it does use the explicit target
    // to model pointer widths and ABI-sensitive Win32 layouts correctly.
    arguments.push(format!("--target={WIN32_TARGET}"));
    arguments.push("/c".to_string());
    arguments.push(source.to_string_lossy().into_owned());
    arguments
}

fn collect_cpp_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to enumerate C++ sources under {}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("failed to read an entry under {}: {e}", dir.display()))
            .path();
        if path.is_dir() {
            collect_cpp_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "cpp") {
            out.push(path);
        }
    }
}

fn resolve_dxsdk() -> PathBuf {
    if let Ok(dir) = env::var("DXSDK_DIR") {
        let path = PathBuf::from(dir);
        if path.join("include/d3dx9.h").exists() {
            return path;
        }
    }

    let default = PathBuf::from(r"C:\Program Files (x86)\Microsoft DirectX SDK (June 2010)");
    if default.join("include/d3dx9.h").exists() {
        return default;
    }

    panic!(
        "DirectX SDK (June 2010) not found. Set DXSDK_DIR to its root or install it at {}",
        default.display()
    );
}

fn cargo() -> Command {
    Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

fn run(cmd: &mut Command) {
    let status = cmd.status().expect("failed to spawn cargo");
    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
}

fn cmd_build(root: &Path, mode: BuildMode) {
    let dll_profile = mode.dll_profile();
    let exe_profile = mode.exe_profile();

    let mut dll_cmd = cargo();
    dll_cmd
        .arg("build")
        .arg("-p")
        .arg("d3d8")
        .arg("-p")
        .arg("dinput8")
        .arg("--target")
        .arg("i686-pc-windows-msvc")
        .arg("--profile")
        .arg(dll_profile);
    if matches!(mode, BuildMode::MaxPerf) {
        // Cargo's lto/codegen-units do not reach the C++; this is what turns on
        // /GL + /LTCG so `--max-perf` actually affects d3d8.dll.
        dll_cmd.arg("--features").arg("d3d8/max-perf");
    }
    dll_cmd.current_dir(root);
    run(&mut dll_cmd);

    let mut exe_cmd = cargo();
    exe_cmd
        .arg("build")
        .arg("-p")
        .arg("MGEXEgui")
        .arg("-p")
        .arg("mgeHost64")
        .arg("--target")
        .arg("x86_64-pc-windows-msvc")
        .arg("--profile")
        .arg(exe_profile)
        .current_dir(root);
    run(&mut exe_cmd);

    let out_dir = root.join("bin").join(format!("MSVC-{}", mode.config_name()));
    fs::create_dir_all(&out_dir).unwrap();

    let dll_target_dir = root.join("target/i686-pc-windows-msvc").join(target_subdir(dll_profile));
    let exe_target_dir = root.join("target/x86_64-pc-windows-msvc").join(target_subdir(exe_profile));

    let artifacts: &[(&Path, &str)] = &[
        (&dll_target_dir, "d3d8.dll"),
        (&dll_target_dir, "d3d8.pdb"),
        (&dll_target_dir, "dinput8.dll"),
        (&dll_target_dir, "dinput8.pdb"),
        (&exe_target_dir, "mgeHost64.exe"),
        (&exe_target_dir, "mgeHost64.pdb"),
        (&exe_target_dir, "MGEXEgui.exe"),
    ];

    for (dir, name) in artifacts {
        let src = dir.join(name);
        if src.exists() {
            fs::copy(&src, out_dir.join(name)).unwrap();
        }
    }

    // bin\MSVC-* doubles as the shareable archive, so it carries the asset tree
    // next to the binaries. mirror_dir only rewrites files that actually changed.
    mirror_dir(&root.join("assets"), &out_dir);
    copy_readme(root, &out_dir);

    println!("Artifacts and assets collected into {}", out_dir.display());
}

/// The release archive: `--max-perf` binaries plus assets, packed flat so it
/// extracts straight into a Morrowind directory.
///
/// Always `--max-perf`; the build-mode flags are ignored.
fn cmd_dist(root: &Path) {
    let mode = BuildMode::MaxPerf;
    cmd_build(root, mode);

    // Stage into a directory rebuilt from scratch rather than packing bin\MSVC-*
    // directly. `mirror_dir` only ever adds, so that directory keeps files that
    // assets/ has since dropped; staging fresh is what keeps them out of a release.
    let staging = root.join("target/dist-staging");
    if staging.exists() {
        fs::remove_dir_all(&staging).unwrap_or_else(|e| panic!("failed to clear {}: {e}", staging.display()));
    }
    fs::create_dir_all(&staging).unwrap();

    // PDBs are deliberately not shipped; crash reports come back as raw addresses.
    let built = root.join("bin").join(format!("MSVC-{}", mode.config_name()));
    for name in ["d3d8.dll", "dinput8.dll", "mgeHost64.exe", "MGEXEgui.exe"] {
        let src = built.join(name);
        if !src.exists() {
            eprintln!("error: {} was not produced by the build", src.display());
            process::exit(1);
        }
        fs::copy(&src, staging.join(name)).unwrap();
    }
    mirror_dir(&root.join("assets"), &staging);
    copy_readme(root, &staging);

    let out_dir = root.join("dist");
    fs::create_dir_all(&out_dir).unwrap();
    let archive = out_dir.join(format!("MGE-XE-G7-v{}.7z", env!("CARGO_PKG_VERSION")));
    // 7-Zip refuses to overwrite entries in an existing archive the way `a` implies,
    // so a stale file would silently merge with the new one.
    if archive.exists() {
        fs::remove_file(&archive).unwrap_or_else(|e| panic!("failed to replace {}: {e}", archive.display()));
    }

    // Every top-level entry is named explicitly, from inside the staging directory,
    // so the archive holds no wrapper folder and no "./" prefixes.
    let mut pack = Command::new("7z");
    pack.arg("a").arg("-t7z").arg("-mx=9").arg(&archive);
    for entry in fs::read_dir(&staging).unwrap() {
        pack.arg(entry.unwrap().file_name());
    }
    pack.current_dir(&staging);
    let status = pack.status().unwrap_or_else(|e| {
        eprintln!("error: could not run 7z: {e}\n7-Zip must be installed and on PATH.");
        process::exit(1);
    });
    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }

    let size = fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
    println!("Packed {} ({:.1} MiB)", archive.display(), size as f64 / (1024.0 * 1024.0));
}

fn cmd_deploy(root: &Path, mode: BuildMode, morrowind_dir: Option<PathBuf>) {
    let morrowind = morrowind_dir
        .or_else(|| env::var("MGE_XE_MORROWIND_DIR").ok().map(PathBuf::from))
        .or_else(|| resolve_from_local_toml(root))
        .unwrap_or_else(|| {
            eprintln!(
                "error: Morrowind directory not configured.\n\
                 Pass --morrowind-dir <path>, set MGE_XE_MORROWIND_DIR, or create\n\
                 mge-xe-local.toml at the repo root with:\n\
                 \x20 morrowind_dir = \"C:/path/to/Morrowind\""
            );
            process::exit(1);
        });

    cmd_build(root, mode);

    let config = mode.config_name();
    let src_bin = root.join("bin").join(format!("MSVC-{config}"));

    let binaries = [
        "d3d8.dll",
        "d3d8.pdb",
        "dinput8.dll",
        "dinput8.pdb",
        "mgeHost64.exe",
        "mgeHost64.pdb",
        "MGEXEgui.exe",
    ];
    for name in &binaries {
        let src = src_bin.join(name);
        if src.exists() {
            fs::copy(&src, morrowind.join(name)).unwrap();
        }
    }

    mirror_dir(&root.join("assets"), &morrowind);
    copy_readme(root, &morrowind);

    println!("Deployed to {}", morrowind.display());
}

fn resolve_from_local_toml(root: &Path) -> Option<PathBuf> {
    let path = root.join("mge-xe-local.toml");
    let content = fs::read_to_string(&path).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("morrowind_dir") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let val = rest.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return Some(PathBuf::from(val));
                }
            }
        }
    }
    None
}

/// Ship the repo root README next to the binaries, renamed: the destination is a
/// shared Morrowind directory where a bare README.md would collide with other mods.
fn copy_readme(root: &Path, dest: &Path) {
    let src = root.join("README.md");
    if src.exists() {
        fs::copy(&src, dest.join("MGE XE Readme.md")).unwrap();
    }
}

/// Copy files from src into dest, creating subdirectories as needed.
/// Skips files that already exist with the same or newer modification time.
fn mirror_dir(src: &Path, dest: &Path) {
    let entries = match fs::read_dir(src) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if let Some(target) = link_pointing_outside(&src_path, &dest_path) {
            eprintln!(
                "warning: not updating {} - it links to {}, which is outside the source tree",
                dest_path.display(),
                target.display()
            );
            continue;
        }
        if src_path.is_dir() {
            fs::create_dir_all(&dest_path).unwrap();
            mirror_dir(&src_path, &dest_path);
        } else if should_copy(&src_path, &dest_path) {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::copy(&src_path, &dest_path).unwrap();
        }
    }
}

/// `Some(target)` when `dest` is a symlink or junction that does not resolve back
/// to `src`.
///
/// Copying onto such a path would follow the link and truncate whatever sits at the
/// far end, in a directory the user never named. Links that do resolve back to `src`
/// are the ordinary "edit the asset in place" convenience setup, so they stay on the
/// normal path, where the timestamp/size comparison skips them anyway.
///
/// Hard links are indistinguishable from regular files here and are still followed.
fn link_pointing_outside(src: &Path, dest: &Path) -> Option<PathBuf> {
    if !fs::symlink_metadata(dest).ok()?.file_type().is_symlink() {
        return None;
    }
    let resolves_to_src = match (fs::canonicalize(src), fs::canonicalize(dest)) {
        (Ok(src), Ok(dest)) => src == dest,
        // A link that cannot be resolved - broken, or a target we cannot open - is
        // precisely the case not to write through.
        _ => false,
    };
    if resolves_to_src {
        return None;
    }
    Some(fs::read_link(dest).unwrap_or_else(|_| dest.to_path_buf()))
}

fn should_copy(src: &Path, dest: &Path) -> bool {
    let src_meta = match fs::metadata(src) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let dest_meta = match fs::metadata(dest) {
        Ok(m) => m,
        Err(_) => return true,
    };
    let src_time = src_meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let dest_time = dest_meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    src_time > dest_time || src_meta.len() != dest_meta.len()
}
