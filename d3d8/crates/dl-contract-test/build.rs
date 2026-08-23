use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let test_file = manifest_dir.join("cpp/output_contract_test.cpp");
    let d3d8_cpp_dir = manifest_dir.join("../../cpp");
    let version_header = d3d8_cpp_dir.join("mge/mgeversion.h");

    println!("cargo:rerun-if-changed={}", test_file.display());
    println!("cargo:rerun-if-changed={}", version_header.display());

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .flag("/GR-")
        .define("WIN32", None)
        .define("NOMINMAX", None)
        .define("NDEBUG", None)
        .include(&d3d8_cpp_dir)
        .file(&test_file)
        .compile("dl_contract_test");
}
