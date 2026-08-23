use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::ptr;

use crate::{ConfigDocument, OpenState};

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FfiStatus {
    Ok = 0,
    MissingDefaults = 1,
    InvalidDefaults = 2,
    InvalidArgument = 3,
    UnknownPath = 4,
    BufferTooSmall = 5,
    ReloadFailed = 6,
    SaveFailed = 7,
}

pub struct FfiDocument {
    config: ConfigDocument,
    last_error: String,
}

impl FfiDocument {
    fn set_error(&mut self, error: impl ToString) {
        self.last_error = error.to_string();
    }

    fn set_warnings(&mut self) {
        self.last_error = self
            .config
            .warnings()
            .iter()
            .map(|warning| format!("{}: {}", warning.path, warning.message))
            .collect::<Vec<_>>()
            .join("; ");
    }
}

unsafe fn path_from_ptr(path: *const c_char) -> Result<PathBuf, FfiStatus> {
    if path.is_null() {
        return Err(FfiStatus::InvalidArgument);
    }
    // SAFETY: The caller owns the NUL-terminated string for the duration of the call.
    let path = unsafe { CStr::from_ptr(path) }
        .to_str()
        .map_err(|_| FfiStatus::InvalidArgument)?;
    Ok(PathBuf::from(path))
}

unsafe fn string_from_ptr(value: *const c_char) -> Result<String, FfiStatus> {
    if value.is_null() {
        return Err(FfiStatus::InvalidArgument);
    }
    // SAFETY: The caller owns the NUL-terminated string for the duration of the call.
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| FfiStatus::InvalidArgument)
}

unsafe fn write_bytes(output: *mut c_char, capacity: usize, bytes: &[u8]) -> Result<(), FfiStatus> {
    if output.is_null() || capacity == 0 {
        return Err(FfiStatus::InvalidArgument);
    }
    if bytes.len() + 1 > capacity {
        return Err(FfiStatus::BufferTooSmall);
    }
    // SAFETY: Capacity was checked and the regions cannot overlap.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast::<u8>(), bytes.len());
        *output.add(bytes.len()) = 0;
    }
    Ok(())
}

/// Opens and validates the complete configuration document.
///
/// # Safety
/// `path` is a NUL-terminated UTF-8 path and `out_document` is writable.
pub unsafe fn open(path: *const c_char, out_document: *mut *mut FfiDocument) -> FfiStatus {
    if out_document.is_null() {
        return FfiStatus::InvalidArgument;
    }
    // Leave output predictably null on every failure.
    unsafe { *out_document = ptr::null_mut() };
    // SAFETY: Forwarding the documented ABI contract.
    let path = match unsafe { path_from_ptr(path) } {
        Ok(path) => path,
        Err(status) => return status,
    };
    let config = ConfigDocument::open(path);
    let status = match config.state() {
        OpenState::Valid => FfiStatus::Ok,
        OpenState::MissingDefaults => FfiStatus::MissingDefaults,
        OpenState::InvalidDefaults => FfiStatus::InvalidDefaults,
    };
    let last_error = config.diagnostic().map(str::to_owned).unwrap_or_else(|| {
        config
            .warnings()
            .iter()
            .map(|warning| format!("{}: {}", warning.path, warning.message))
            .collect::<Vec<_>>()
            .join("; ")
    });
    let document = Box::new(FfiDocument { config, last_error });
    // SAFETY: out_document was checked above; ownership transfers to the caller.
    unsafe { *out_document = Box::into_raw(document) };
    status
}

/// Closes a document returned by [`open`].
///
/// # Safety
/// `document` must be null or an unclosed pointer returned by [`open`].
pub unsafe fn close(document: *mut FfiDocument) {
    if !document.is_null() {
        // SAFETY: Ownership is returned exactly once by the caller.
        drop(unsafe { Box::from_raw(document) });
    }
}

/// Gets a numeric runtime mapping without modifying `out_value` on failure.
///
/// # Safety
/// All pointers obey their C ABI contracts for the duration of the call.
pub unsafe fn get_number(document: *const FfiDocument, path: *const c_char, out_value: *mut f64) -> FfiStatus {
    if document.is_null() || out_value.is_null() {
        return FfiStatus::InvalidArgument;
    }
    // SAFETY: Pointers were checked above.
    let path = match unsafe { string_from_ptr(path) } {
        Ok(path) => path,
        Err(status) => return status,
    };
    // SAFETY: The caller retains a live immutable document handle.
    let document = unsafe { &*document };
    let Some(value) = document.config.get_number(&path) else {
        return FfiStatus::UnknownPath;
    };
    // SAFETY: out_value was checked and is only assigned on success.
    unsafe { *out_value = value };
    FfiStatus::Ok
}

/// Sets a numeric runtime mapping.
///
/// # Safety
/// All pointers obey their C ABI contracts for the duration of the call.
pub unsafe fn set_number(document: *mut FfiDocument, path: *const c_char, value: f64) -> FfiStatus {
    if document.is_null() {
        return FfiStatus::InvalidArgument;
    }
    // SAFETY: Pointers were checked above.
    let path = match unsafe { string_from_ptr(path) } {
        Ok(path) => path,
        Err(status) => return status,
    };
    // SAFETY: The caller owns the mutable document handle on one thread.
    let document = unsafe { &mut *document };
    match document.config.set_number(&path, value) {
        Ok(()) => {
            document.set_warnings();
            FfiStatus::Ok
        }
        Err(error) => {
            document.set_error(error);
            FfiStatus::UnknownPath
        }
    }
}

/// Gets a UTF-8 string without truncation.
///
/// # Safety
/// All pointers obey their C ABI contracts for the duration of the call.
pub unsafe fn get_string(
    document: *const FfiDocument,
    path: *const c_char,
    output: *mut c_char,
    capacity: usize,
) -> FfiStatus {
    if document.is_null() {
        return FfiStatus::InvalidArgument;
    }
    // SAFETY: Forwarding the documented pointer contracts.
    let path = match unsafe { string_from_ptr(path) } {
        Ok(path) => path,
        Err(status) => return status,
    };
    // SAFETY: The caller retains a live immutable document handle.
    let document = unsafe { &*document };
    let Some(value) = document.config.get_string(&path) else {
        return FfiStatus::UnknownPath;
    };
    // SAFETY: Forwarding the documented output contract.
    match unsafe { write_bytes(output, capacity, value.as_bytes()) } {
        Ok(()) => FfiStatus::Ok,
        Err(status) => status,
    }
}

/// Sets a UTF-8 string.
///
/// # Safety
/// All pointers obey their C ABI contracts for the duration of the call.
pub unsafe fn set_string(document: *mut FfiDocument, path: *const c_char, value: *const c_char) -> FfiStatus {
    if document.is_null() {
        return FfiStatus::InvalidArgument;
    }
    // SAFETY: Forwarding the documented pointer contracts.
    let path = match unsafe { string_from_ptr(path) } {
        Ok(path) => path,
        Err(status) => return status,
    };
    let value = match unsafe { string_from_ptr(value) } {
        Ok(value) => value,
        Err(status) => return status,
    };
    // SAFETY: The caller owns the mutable document handle on one thread.
    let document = unsafe { &mut *document };
    match document.config.set_string(&path, &value) {
        Ok(()) => {
            document.set_warnings();
            FfiStatus::Ok
        }
        Err(error) => {
            document.set_error(error);
            FfiStatus::UnknownPath
        }
    }
}

/// Renders a legacy double-NUL-terminated line buffer without truncation.
///
/// # Safety
/// All pointers obey their C ABI contracts for the duration of the call.
pub unsafe fn get_lines(
    document: *const FfiDocument,
    path: *const c_char,
    output: *mut c_char,
    capacity: usize,
) -> FfiStatus {
    if document.is_null() || output.is_null() || capacity == 0 {
        return FfiStatus::InvalidArgument;
    }
    // SAFETY: Forwarding the documented pointer contracts.
    let path = match unsafe { string_from_ptr(path) } {
        Ok(path) => path,
        Err(status) => return status,
    };
    // SAFETY: The caller retains a live immutable document handle.
    let document = unsafe { &*document };
    let Some(lines) = document.config.get_lines(&path) else {
        return FfiStatus::UnknownPath;
    };
    let content_bytes = lines.iter().map(|line| line.len() + 1).sum::<usize>();
    let required = content_bytes + if content_bytes == 0 { 2 } else { 1 };
    if required > capacity {
        return FfiStatus::BufferTooSmall;
    }
    let mut cursor = 0;
    for line in lines {
        // SAFETY: required was checked and line bytes do not overlap output.
        unsafe {
            ptr::copy_nonoverlapping(line.as_ptr(), output.add(cursor).cast::<u8>(), line.len());
            cursor += line.len();
            *output.add(cursor) = 0;
            cursor += 1;
        }
    }
    // SAFETY: required includes this final terminator.
    unsafe {
        *output.add(cursor) = 0;
        if cursor == 0 {
            *output.add(1) = 0;
        }
    };
    FfiStatus::Ok
}

/// Transactionally reloads a repaired/changed live document.
///
/// # Safety
/// `document` is a live mutable handle owned by the calling thread.
pub unsafe fn reload(document: *mut FfiDocument) -> FfiStatus {
    if document.is_null() {
        return FfiStatus::InvalidArgument;
    }
    // SAFETY: The caller owns the mutable document handle on one thread.
    let document = unsafe { &mut *document };
    match document.config.reload() {
        Ok(()) => {
            document.set_warnings();
            FfiStatus::Ok
        }
        Err(error) => {
            document.set_error(error);
            FfiStatus::ReloadFailed
        }
    }
}

/// Saves runtime-owned forced paths, refusing a stale document revision.
///
/// # Safety
/// `document` is a live mutable handle owned by the calling thread.
pub unsafe fn save(document: *mut FfiDocument) -> FfiStatus {
    if document.is_null() {
        return FfiStatus::InvalidArgument;
    }
    // SAFETY: The caller owns the mutable document handle on one thread.
    let document = unsafe { &mut *document };
    match document.config.save() {
        Ok(()) => {
            document.set_warnings();
            FfiStatus::Ok
        }
        Err(error) => {
            document.set_error(error);
            FfiStatus::SaveFailed
        }
    }
}

/// Returns 1 only while first-run creation is still permitted.
///
/// # Safety
/// `document` is a live immutable handle.
pub unsafe fn needs_creation(document: *const FfiDocument) -> u32 {
    if document.is_null() {
        return 0;
    }
    // SAFETY: The caller retains a live immutable document handle.
    u32::from(unsafe { &*document }.config.needs_creation())
}

/// Copies the handle-local diagnostic without truncation.
///
/// # Safety
/// All pointers obey their C ABI contracts for the duration of the call.
pub unsafe fn last_error(document: *const FfiDocument, output: *mut c_char, capacity: usize) -> FfiStatus {
    if document.is_null() {
        return FfiStatus::InvalidArgument;
    }
    // SAFETY: The caller retains a live immutable document handle.
    let document = unsafe { &*document };
    // SAFETY: Forwarding the documented output contract.
    match unsafe { write_bytes(output, capacity, document.last_error.as_bytes()) } {
        Ok(()) => FfiStatus::Ok,
        Err(status) => status,
    }
}
