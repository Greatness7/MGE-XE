use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let cpp_dir = manifest_dir.join("cpp");
    let def_file = cpp_dir.join("exports.def");

    println!("cargo:rerun-if-changed={}", cpp_dir.display());
    println!("cargo:rerun-if-changed={}", def_file.display());

    let is_release = env::var("OPT_LEVEL").is_ok_and(|l| l != "0");

    let mut sources: Vec<PathBuf> = Vec::new();
    collect_cpp_files(&cpp_dir, &mut sources);
    sources.sort();

    // `main.cpp` defines `DllMain`, and it must reach the linker as a direct
    // object file rather than as a member of the `mge_d3d8` archive. link.exe
    // refuses to let an archive member override msvcrt's `dll_dllmain_stub.obj`
    // (LNK2005 _DllMain@12) while accepting a plain object that does the same,
    // so keeping it out of the archive is what makes link.exe -- and therefore
    // /GL + /LTCG -- usable at all.
    let entry_source = cpp_dir.join("main.cpp");
    let entry_sources: Vec<PathBuf> = sources.iter().filter(|p| *p == &entry_source).cloned().collect();
    assert_eq!(entry_sources.len(), 1, "expected exactly one cpp/main.cpp defining DllMain");
    sources.retain(|p| p != &entry_source);

    let dxsdk = resolve_dxsdk();

    // Whole-program optimization: /GL defers codegen to link time so the
    // optimizer can inline across translation units, which is the C++ analogue
    // of Cargo's `lto = "fat"` plus `codegen-units = 1`. Requires link.exe --
    // rust-lld cannot consume /GL objects -- and requires /LTCG at link time.
    let whole_program = env::var_os("CARGO_FEATURE_MAX_PERF").is_some();
    if whole_program {
        println!("cargo:rustc-link-arg=/LTCG");
    }

    let mut build = base_build(&cpp_dir, &dxsdk, is_release, whole_program);
    for source in &sources {
        build.file(source);
    }
    build.compile("mge_d3d8");

    let mut entry_build = base_build(&cpp_dir, &dxsdk, is_release, whole_program);
    for source in &entry_sources {
        entry_build.file(source);
    }
    for obj in entry_build.compile_intermediates() {
        println!("cargo:rustc-link-arg={}", obj.display());
    }

    println!("cargo:rustc-link-search=native={}", dxsdk.join("Lib\\x86").display());
    println!("cargo:rustc-link-arg=/DEF:{}", def_file.display());
    println!("cargo:rustc-link-lib=kernel32");
    println!("cargo:rustc-link-lib=user32");
    println!("cargo:rustc-link-lib=gdi32");
    println!("cargo:rustc-link-lib=d3d9");
    println!("cargo:rustc-link-lib=d3dx9");
}

/// Compiler configuration shared by the archive and the standalone entry-point
/// object; the two must agree on every flag.
fn base_build(cpp_dir: &Path, dxsdk: &Path, is_release: bool, whole_program: bool) -> cc::Build {
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        // cc does NOT supply /EHsc; without it MSVC compiles with unwind
        // semantics disabled (C4530) and drops the EH runtime imports the
        // MSBuild build has. MGE-XE.vcxproj leaves ExceptionHandling unset,
        // which means the MSBuild default of Sync (/EHsc).
        .flag("/EHsc")
        .flag("/arch:SSE2")
        .flag("/fp:fast")
        .flag("/GR-")
        .flag("/Z7")
        .define("WIN32", None)
        .define("NOMINMAX", None)
        .define("_WINDOWS", None)
        .include(cpp_dir)
        .include(dxsdk.join("include"));

    if is_release {
        build.define("NDEBUG", None);
    }
    // `cc` uses the release dynamic CRT (/MD) for MSVC targets. Defining
    // `_DEBUG` with /MD enables debug-CRT STL machinery that cannot link.
    // Leaving both macros unset keeps ordinary assertions active in dev.

    if whole_program {
        build.flag("/GL");

        // /O2 already implies /Ob2; /Ob3 raises the inlining budget further,
        // mainly through nested call chains. Purely a heuristic change -- no
        // semantic or safety difference -- but it grows the code footprint and
        // can cost instruction-cache locality in the render loop.
        build.flag("/Ob3");

        // Drops the /GS stack cookie: the guard value placed between local
        // buffers and the return address, checked on function exit. Removing
        // it saves a load/compare/branch in every function with stack buffers,
        // but stack overruns stop failing loudly and become silent corruption
        // -- and MGE parses files that arrive from untrusted mod archives.
        // Left off deliberately; enable only with that tradeoff in mind.
        // build.flag("/GS-");
    }

    build
}

fn collect_cpp_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_cpp_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "cpp") {
            out.push(path);
        }
    }
}

fn resolve_dxsdk() -> PathBuf {
    if let Ok(dir) = env::var("DXSDK_DIR") {
        let p = PathBuf::from(dir);
        if p.join("include").join("d3dx9.h").exists() {
            return p;
        }
    }
    let known = PathBuf::from(r"C:\Program Files (x86)\Microsoft DirectX SDK (June 2010)");
    if known.join("include").join("d3dx9.h").exists() {
        return known;
    }
    panic!(
        "DirectX SDK (June 2010) not found. Set DXSDK_DIR to its root or install it to the \
         default path: C:\\Program Files (x86)\\Microsoft DirectX SDK (June 2010)"
    );
}
