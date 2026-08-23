use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let test_file = manifest_dir.join("cpp/config_contract.cpp");
    let d3d8_cpp_dir = manifest_dir.join("../../cpp");

    println!("cargo:rerun-if-changed={}", test_file.display());
    println!(
        "cargo:rerun-if-changed={}",
        d3d8_cpp_dir.join("mge/configuration.cpp").display()
    );
    println!("cargo:rerun-if-changed={}", d3d8_cpp_dir.join("mge/inidata.h").display());
    println!(
        "cargo:rerun-if-changed={}",
        d3d8_cpp_dir.join("mge/configinternal.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        d3d8_cpp_dir.join("mge/configuration.h").display()
    );

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .flag("/GR-")
        .define("WIN32", None)
        .define("NOMINMAX", None)
        .define("NDEBUG", None)
        .include(&d3d8_cpp_dir)
        .file(&test_file)
        .compile("config_contract_test");
}
