use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let cpp_dir = manifest_dir.join("cpp");
    let def_file = cpp_dir.join("exports.def");

    println!("cargo:rerun-if-changed={}", cpp_dir.display());
    println!("cargo:rerun-if-changed={}", def_file.display());

    let profile = env::var("PROFILE").unwrap();
    let is_release = profile == "release" || profile == "release-dll";

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        // Match the MSBuild default of ExceptionHandling=Sync; see d3d8.
        .flag("/EHsc")
        .flag("/GR-")
        .define("WIN32", None)
        .define("DINPUT8_EXPORTS", None)
        .define("_WINDOWS", None)
        .define("_USRDLL", None)
        .file(cpp_dir.join("dinput.cpp"));

    if is_release {
        build.define("NDEBUG", None);
    }
    // `cc` uses the release dynamic CRT (/MD) for MSVC targets. Defining
    // `_DEBUG` with /MD enables debug-CRT STL machinery that cannot link.
    // Leaving both macros unset keeps ordinary assertions active in dev.

    build.compile("dinput_shim");

    println!("cargo:rustc-link-arg=/DEF:{}", def_file.display());
    println!("cargo:rustc-link-lib=kernel32");
}
