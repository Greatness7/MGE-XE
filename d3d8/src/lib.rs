use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::Once;

use mge_config::ffi::FfiDocument;

unsafe extern "C" {
    fn mge_log_panic(message: *const c_char);
}

fn install_panic_hook() {
    static INSTALL: Once = Once::new();

    // Release builds abort, so preserve the diagnostic before Windows terminates the game.
    INSTALL.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let mut text = format!("Rust {info}");
            text.retain(|character| character != '\0');
            if let Ok(message) = CString::new(text) {
                // SAFETY: message remains live for the duration of the call.
                unsafe { mge_log_panic(message.as_ptr()) };
            }
        }));
    });
}

/// Opens the configuration document at `path`.
///
/// # Safety
/// - `path` must be a valid, NUL-terminated C string pointer or null.
/// - `out_document` must be a valid, writable pointer to receive the document handle pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_open(path: *const c_char, out_document: *mut *mut FfiDocument) -> u32 {
    install_panic_hook();
    unsafe { mge_config::ffi::open(path, out_document) as u32 }
}

/// Closes and frees a configuration document created by [`mge_config_open`].
///
/// # Safety
/// - `document` must be a valid pointer returned by [`mge_config_open`] or null.
/// - The document pointer must not be used after being passed to this function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_close(document: *mut FfiDocument) {
    unsafe { mge_config::ffi::close(document) }
}

/// Retrieves a numeric value at key `path`.
///
/// # Safety
/// - `document` must be a valid pointer to an open [`FfiDocument`].
/// - `path` must be a valid, NUL-terminated C string pointer.
/// - `out_value` must be a valid, writable pointer for an `f64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_get_num(document: *const FfiDocument, path: *const c_char, out_value: *mut f64) -> u32 {
    unsafe { mge_config::ffi::get_number(document, path, out_value) as u32 }
}

/// Sets a numeric value at key `path`.
///
/// # Safety
/// - `document` must be a valid pointer to an open [`FfiDocument`].
/// - `path` must be a valid, NUL-terminated C string pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_set_num(document: *mut FfiDocument, path: *const c_char, value: f64) -> u32 {
    unsafe { mge_config::ffi::set_number(document, path, value) as u32 }
}

/// Retrieves a string value at key `path`.
///
/// # Safety
/// - `document` must be a valid pointer to an open [`FfiDocument`].
/// - `path` must be a valid, NUL-terminated C string pointer.
/// - `output` must point to a buffer of at least `capacity` bytes capable of receiving the string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_get_str(
    document: *const FfiDocument,
    path: *const c_char,
    output: *mut c_char,
    capacity: usize,
) -> u32 {
    unsafe { mge_config::ffi::get_string(document, path, output, capacity) as u32 }
}

/// Sets a string value at key `path`.
///
/// # Safety
/// - `document` must be a valid pointer to an open [`FfiDocument`].
/// - `path` and `value` must be valid, NUL-terminated C string pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_set_str(document: *mut FfiDocument, path: *const c_char, value: *const c_char) -> u32 {
    unsafe { mge_config::ffi::set_string(document, path, value) as u32 }
}

/// Retrieves a list of lines/strings at key `path` as newline-separated content.
///
/// # Safety
/// - `document` must be a valid pointer to an open [`FfiDocument`].
/// - `path` must be a valid, NUL-terminated C string pointer.
/// - `output` must point to a buffer of at least `capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_get_lines(
    document: *const FfiDocument,
    path: *const c_char,
    output: *mut c_char,
    capacity: usize,
) -> u32 {
    unsafe { mge_config::ffi::get_lines(document, path, output, capacity) as u32 }
}

/// Reloads the configuration document from disk.
///
/// # Safety
/// - `document` must be a valid pointer to an open [`FfiDocument`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_reload(document: *mut FfiDocument) -> u32 {
    unsafe { mge_config::ffi::reload(document) as u32 }
}

/// Saves the configuration document to disk.
///
/// # Safety
/// - `document` must be a valid pointer to an open [`FfiDocument`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_save(document: *mut FfiDocument) -> u32 {
    unsafe { mge_config::ffi::save(document) as u32 }
}

/// Checks if the document was newly created/missing defaults.
///
/// # Safety
/// - `document` must be a valid pointer to an open [`FfiDocument`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_needs_creation(document: *const FfiDocument) -> u32 {
    unsafe { mge_config::ffi::needs_creation(document) }
}

/// Copies the last error message into `output`.
///
/// # Safety
/// - `document` must be a valid pointer to an open [`FfiDocument`].
/// - `output` must point to a buffer of at least `capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mge_config_last_error(document: *const FfiDocument, output: *mut c_char, capacity: usize) -> u32 {
    unsafe { mge_config::ffi::last_error(document, output, capacity) as u32 }
}
