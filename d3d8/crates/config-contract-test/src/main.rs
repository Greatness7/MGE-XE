use std::collections::BTreeSet;
use std::ffi::CStr;
use std::fs;
use std::os::raw::{c_char, c_ulong};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::{SystemTime, UNIX_EPOCH};

use mge_config::RuntimeNumberRepr;

#[path = "../../../src/lib.rs"]
mod d3d8_exports;

const T_BIT: u32 = 0;
const T_BOOL: u32 = 1;
const T_UINT8: u32 = 2;
const T_INT8: u32 = 3;
const T_UINT16: u32 = 4;
const T_INT16: u32 = 5;
const T_UINT32: u32 = 6;
const T_INT32: u32 = 7;
const T_FLOAT: u32 = 8;
const T_DOUBLE: u32 = 9;
const T_STRING: u32 = 10;
const T_SET: u32 = 11;
const DONT_SAVE: c_ulong = 1 << 7;

unsafe extern "C" {
    fn mge_config_contract_binding_count() -> usize;
    fn mge_config_contract_binding_path(index: usize) -> *const c_char;
    fn mge_config_contract_binding_type(index: usize) -> u32;
    fn mge_config_contract_binding_size(index: usize) -> usize;
    fn mge_config_contract_binding_flags(index: usize) -> c_ulong;
    fn mge_config_contract_binding_number(index: usize) -> f64;
    fn mge_config_contract_binding_buffer(index: usize) -> *const c_char;
    fn mge_config_contract_load_defaults() -> i32;
    fn mge_config_contract_configuration_address() -> *const u8;
    fn mge_config_contract_dl_address() -> *const u8;
    fn mge_config_contract_set_camera_zoom(value: f32);
    fn mge_config_contract_camera_zoom() -> f32;
}

fn main() {
    let original_dir = std::env::current_dir().expect("current directory");
    let root = temporary_root();
    fs::create_dir(&root).expect("create contract-test directory");
    std::env::set_current_dir(&root).expect("enter contract-test directory");

    // SAFETY: The linked C++ harness exposes a fixed, process-local table and
    // loads the shared crate through the same exported ABI as d3d8.dll.
    let loaded = unsafe { mge_config_contract_load_defaults() };
    assert_eq!(loaded, 1, "default config must load through C++");
    let configuration_address =
        NonNull::new(unsafe { mge_config_contract_configuration_address() }.cast_mut()).expect("Configuration address");
    let dl_address = NonNull::new(unsafe { mge_config_contract_dl_address() }.cast_mut()).expect("Configuration.DL address");
    unsafe {
        mge_config_contract_set_camera_zoom(123.5);
        assert_eq!(mge_config_contract_load_defaults(), 1);
        assert_eq!(mge_config_contract_camera_zoom(), 123.5);
        assert_eq!(
            NonNull::new(mge_config_contract_configuration_address().cast_mut()),
            Some(configuration_address)
        );
        assert_eq!(NonNull::new(mge_config_contract_dl_address().cast_mut()), Some(dl_address));
    }

    let expected = mge_config::ConfigDocument::open(root.join(mge_config::FILE_NAME));
    let count = unsafe { mge_config_contract_binding_count() };
    assert_eq!(count, 133, "binding row count changed; update the reviewed inventory");

    let mut seen = BTreeSet::new();
    let mut dont_save = BTreeSet::new();
    let expected_dont_save: BTreeSet<&str> = [
        "runtime.disabled",
        "runtime.mwse_disabled",
        "runtime.proxy_only",
        "distant_land.water_without_land",
        "graphics.anti_aliasing",
        "graphics.z_buffer_format",
        "graphics.vsync",
        "graphics.refresh_rate",
        "graphics.borderless",
        "graphics.anisotropy",
        "render.screenshot_format",
        "render.screenshot_directory",
        "render.screenshot_name",
        "render.screenshot_suffix",
        "shaders.chain",
        "input.macros",
        "input.triggers",
        "input.remap",
    ]
    .into_iter()
    .collect();

    for index in 0..count {
        let path = unsafe {
            CStr::from_ptr(mge_config_contract_binding_path(index))
                .to_str()
                .expect("binding paths are UTF-8")
        };
        assert!(seen.insert(path.to_owned()), "duplicate C++ binding for {path}");

        let kind = unsafe { mge_config_contract_binding_type(index) };
        let size = unsafe { mge_config_contract_binding_size(index) };
        let flags = unsafe { mge_config_contract_binding_flags(index) };
        if flags & DONT_SAVE != 0 {
            dont_save.insert(path);
        }

        match kind {
            T_STRING => {
                let expected_value = expected
                    .get_string(path)
                    .unwrap_or_else(|| panic!("{path}: missing string schema binding"));
                assert!(expected_value.len() < size, "{path} exceeds its C buffer");
                let actual = unsafe { CStr::from_ptr(mge_config_contract_binding_buffer(index)) }
                    .to_str()
                    .expect("string defaults are ASCII");
                assert_eq!(actual, expected_value, "{path} default mismatch");
            }
            T_SET => {
                let lines = expected
                    .get_lines(path)
                    .unwrap_or_else(|| panic!("{path}: missing structured schema binding"));
                let content_bytes = lines.iter().map(|line| line.len() + 1).sum::<usize>();
                let required = content_bytes + if content_bytes == 0 { 2 } else { 1 };
                assert!(required <= size, "{path} exceeds its C multi-string buffer");
                let actual = read_multisz(unsafe { mge_config_contract_binding_buffer(index) }, size);
                assert_eq!(actual, lines, "{path} rendered-line mismatch");
            }
            T_BIT | T_BOOL | T_UINT8 | T_INT8 | T_UINT16 | T_INT16 | T_UINT32 | T_INT32 | T_FLOAT | T_DOUBLE => {
                let expected_value = expected
                    .get_number(path)
                    .unwrap_or_else(|| panic!("{path}: missing numeric schema binding"));
                let repr = expected
                    .settings()
                    .runtime_number_repr(path)
                    .unwrap_or_else(|| panic!("{path}: missing numeric representation"));
                assert_repr(kind, repr, path);
                let actual = unsafe { mge_config_contract_binding_number(index) };
                let tolerance = if matches!(kind, T_FLOAT) {
                    f32::EPSILON as f64 * expected_value.abs().max(1.0) * 4.0
                } else {
                    0.0
                };
                assert!(
                    (actual - expected_value).abs() <= tolerance,
                    "{path}: C++ has {actual}, Rust expects {expected_value}"
                );
            }
            _ => panic!("{path}: unknown C++ storage type {kind}"),
        }
    }

    assert_eq!(
        dont_save, expected_dont_save,
        "DONT_SAVE ownership changed without updating the explicit contract"
    );

    std::env::set_current_dir(&original_dir).expect("restore current directory");
    fs::remove_dir_all(&root).expect("remove contract-test directory");
    println!("config_contract_test: {count} bindings verified");
}

fn temporary_root() -> PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).expect("system clock").as_nanos();
    std::env::temp_dir().join(format!("mge-config-contract-{}-{nonce}", std::process::id()))
}

fn read_multisz(pointer: *const c_char, capacity: usize) -> Vec<String> {
    assert!(!pointer.is_null());
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), capacity) };
    let mut lines = Vec::new();
    let mut start = 0;
    while start < bytes.len() && bytes[start] != 0 {
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|offset| start + offset)
            .expect("multi-string terminator");
        lines.push(
            std::str::from_utf8(&bytes[start..end])
                .expect("rendered lines are UTF-8")
                .to_owned(),
        );
        start = end + 1;
    }
    lines
}

fn assert_repr(kind: u32, repr: RuntimeNumberRepr, path: &str) {
    let compatible = match repr {
        RuntimeNumberRepr::Boolean => matches!(kind, T_BIT | T_BOOL),
        RuntimeNumberRepr::U8 => kind == T_UINT8,
        RuntimeNumberRepr::U32 => kind == T_UINT32,
        RuntimeNumberRepr::I32 => kind == T_INT32,
        RuntimeNumberRepr::F32 => kind == T_FLOAT,
    };
    assert!(
        compatible,
        "{path}: Rust representation {repr:?} is incompatible with C++ storage type {kind}"
    );
}
