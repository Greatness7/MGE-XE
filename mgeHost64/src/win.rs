use std::ffi::{CStr, c_void};
use std::ptr::{NonNull, null, null_mut};
use std::slice;

use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, FALSE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_ABANDONED_0, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Environment::GetCommandLineA;
use windows_sys::Win32::System::Memory::{
    CreateFileMappingA, FILE_MAP_ALL_ACCESS, MEM_COMMIT, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE,
    SEC_RESERVE, UnmapViewOfFile, VirtualAlloc,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows_sys::Win32::System::Threading::{
    CreateEventA, CreateMutexA, GetCurrentProcess, ReleaseMutex, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

use crate::error::HostError;

/// Handles from the 32-bit parent process, passed on the startup command line.
#[derive(Clone, Copy, Debug)]
pub struct StartupHandles {
    pub shared_mem: HANDLE,
    pub client_process: HANDLE,
    pub rpc_start_event: HANDLE,
    pub rpc_complete_event: HANDLE,
}

/// Explains startup-handle failures caused by manual launches or mismatched binaries.
const NOT_LAUNCHED_BY_MGE_HINT: &str = "mgeHost64.exe is launched automatically by MGE-XE's d3d8.dll and is not meant to be run directly; \
     if this appeared after updating MGE XE, your mgeHost64.exe is out of date and should be replaced so \
     it matches d3d8.dll and MGEXEgui.exe.";

/// The parent launches the host with only four handle tokens and no `argv[0]`.
pub fn raw_command_line() -> String {
    let raw = unsafe { GetCommandLineA() };
    if raw.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(raw.cast::<i8>()) }.to_string_lossy().into_owned()
    }
}

/// Parses the four handle tokens supplied by `d3d8.dll`.
pub fn parse_startup_handles() -> Result<StartupHandles, HostError> {
    let command = raw_command_line();
    let command = command.trim();
    if command.is_empty() {
        return Err(HostError::parse_failure(&format!(
            "no command line was provided (GetCommandLineA returned nothing). {NOT_LAUNCHED_BY_MGE_HINT}"
        )));
    }
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() < 4 {
        return Err(HostError::parse_failure(&format!(
            "expected 4 IPC handle arguments (shared_mem client_process rpc_start rpc_complete) but found {} \
             token(s) on the command line {command:?}. {NOT_LAUNCHED_BY_MGE_HINT}",
            parts.len()
        )));
    }

    let shared_mem = parse_handle_token(parts[0], "shared_mem", command)? as HANDLE;
    let client_process = parse_handle_token(parts[1], "client_process", command)? as HANDLE;
    let rpc_start_event = parse_handle_token(parts[2], "rpc_start_event", command)? as HANDLE;
    let rpc_complete_event = parse_handle_token(parts[3], "rpc_complete_event", command)? as HANDLE;

    #[cfg(debug_assertions)]
    unsafe {
        while windows_sys::Win32::System::Diagnostics::Debug::IsDebuggerPresent() == 0 {
            windows_sys::Win32::System::Threading::Sleep(100);
        }
    }

    Ok(StartupHandles {
        shared_mem,
        client_process,
        rpc_start_event,
        rpc_complete_event,
    })
}

fn parse_handle_token(value: &str, field: &str, command: &str) -> Result<usize, HostError> {
    parse_pointer(value).ok_or_else(|| {
        HostError::parse_failure(&format!(
            "could not parse the {field} handle from token {value:?} on the command line {command:?}. \
             {NOT_LAUNCHED_BY_MGE_HINT}"
        ))
    })
}

fn parse_pointer(value: &str) -> Option<usize> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        usize::from_str_radix(hex, 16).ok()
    } else if trimmed.chars().all(|ch| ch.is_ascii_hexdigit()) {
        usize::from_str_radix(trimmed, 16).ok()
    } else {
        trimmed.parse::<usize>().ok()
    }
}

pub fn allocation_granularity() -> u32 {
    let mut info = SYSTEM_INFO::default();
    unsafe { GetSystemInfo(&raw mut info) };
    info.dwAllocationGranularity
}

pub fn set_event(handle: HANDLE) -> Result<(), HostError> {
    let ok = unsafe { SetEvent(handle) };
    if ok == 0 {
        return Err(HostError::win32("Failed to signal event", unsafe { GetLastError() }));
    }
    Ok(())
}

pub fn wait_multiple(handles: &[HANDLE], milliseconds: u32) -> Result<u32, HostError> {
    let result = unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), FALSE, milliseconds) };
    if result == WAIT_FAILED {
        return Err(HostError::win32("Failed to wait for event", unsafe { GetLastError() }));
    }
    Ok(result)
}

pub fn map_view(handle: HANDLE, offset: u32, bytes: usize) -> Result<*mut c_void, HostError> {
    let pointer = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, offset, bytes) };
    if pointer.Value.is_null() {
        return Err(HostError::win32("MapViewOfFile failed", unsafe { GetLastError() }));
    }
    Ok(pointer.Value)
}

pub fn commit_pages(address: *mut c_void, bytes: usize) -> Result<(), HostError> {
    let pointer = unsafe { VirtualAlloc(address, bytes, MEM_COMMIT, PAGE_READWRITE) };
    if pointer.is_null() {
        return Err(HostError::win32("VirtualAlloc failed", unsafe { GetLastError() }));
    }
    Ok(())
}

pub fn create_reserved_mapping(total_bytes: u32) -> Result<HANDLE, HostError> {
    let handle = unsafe {
        CreateFileMappingA(
            INVALID_HANDLE_VALUE,
            null(),
            PAGE_READWRITE | SEC_RESERVE,
            0,
            total_bytes,
            null(),
        )
    };
    if handle.is_null() {
        return Err(HostError::win32("CreateFileMappingA failed", unsafe { GetLastError() }));
    }
    Ok(handle)
}

pub fn create_auto_reset_event() -> Result<HANDLE, HostError> {
    let handle = unsafe { CreateEventA(null(), FALSE, FALSE, null()) };
    if handle.is_null() {
        return Err(HostError::win32("CreateEventA failed", unsafe { GetLastError() }));
    }
    Ok(handle)
}

pub struct NamedMutex {
    handle: OwnedHandle,
}

impl NamedMutex {
    pub fn create(name: &str) -> Result<Self, HostError> {
        let mut bytes = name.as_bytes().to_vec();
        bytes.push(0);
        let handle = unsafe { CreateMutexA(null(), FALSE, bytes.as_ptr()) };
        if handle.is_null() {
            return Err(HostError::win32("CreateMutexA failed", unsafe { GetLastError() }));
        }
        Ok(Self {
            handle: OwnedHandle(handle),
        })
    }

    pub fn try_acquire(&self) -> Result<Option<NamedMutexGuard<'_>>, HostError> {
        self.acquire_for(0)
    }

    pub fn acquire_for(&self, milliseconds: u32) -> Result<Option<NamedMutexGuard<'_>>, HostError> {
        match unsafe { WaitForSingleObject(self.handle.raw(), milliseconds) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED_0 => Ok(Some(NamedMutexGuard { mutex: self })),
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(HostError::win32("WaitForSingleObject failed", unsafe { GetLastError() })),
            _ => Err(HostError::win32("WaitForSingleObject returned unexpected status", unsafe {
                GetLastError()
            })),
        }
    }
}

pub struct NamedMutexGuard<'a> {
    mutex: &'a NamedMutex,
}

impl Drop for NamedMutexGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.mutex.handle.raw());
        }
    }
}

pub fn duplicate_to_process(source: HANDLE, target_process: HANDLE) -> Result<HANDLE, HostError> {
    let mut duplicate = null_mut();
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source,
            target_process,
            &raw mut duplicate,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(HostError::win32("DuplicateHandle failed", unsafe { GetLastError() }));
    }
    Ok(duplicate)
}

/// Errors are ignored during best-effort remote cleanup.
pub fn close_remote_handle(target_process: HANDLE, remote: u32) {
    let mut dummy = null_mut();
    unsafe {
        DuplicateHandle(
            target_process,
            remote as usize as HANDLE,
            target_process,
            &raw mut dummy,
            0,
            FALSE,
            DUPLICATE_CLOSE_SOURCE,
        );
    }
}

pub struct OwnedHandle(pub HANDLE);

impl OwnedHandle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub struct MappedView {
    ptr: NonNull<u8>,
    len: usize,
}

impl MappedView {
    pub fn new(ptr: *mut c_void, len: usize) -> Result<Self, HostError> {
        let ptr = NonNull::new(ptr.cast::<u8>()).ok_or_else(|| HostError::init("Mapped view pointer was null"))?;
        Ok(Self { ptr, len })
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr.as_ptr().cast()
    }

    /// The peer may mutate this mapping under the event protocol.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// Callers must synchronize with the peer before accessing this mapping.
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    pub fn ptr_at(&self, offset: usize) -> Result<*mut c_void, HostError> {
        if offset > self.len {
            return Err(HostError::init("Mapped view offset out of range"));
        }
        Ok(unsafe { self.ptr.as_ptr().add(offset).cast() })
    }
}

impl Drop for MappedView {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ptr.as_ptr().cast(),
            })
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pointer_style_handles() {
        assert_eq!(parse_pointer("0x1234").unwrap(), 0x1234);
        assert_eq!(parse_pointer("00000000000000FF").unwrap(), 0xFF);
    }
}
