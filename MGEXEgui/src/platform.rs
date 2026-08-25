use std::{ffi::OsStr, io, path::Path, process::Command};

use anyhow::{Context, Result};
use rust_i18n::t;
use sysinfo::{ProcessRefreshKind, RefreshKind, System};
use windows::Win32::Graphics::Direct3D9::{D3D_SDK_VERSION, D3DADAPTER_DEFAULT, D3DCAPS9, D3DDEVTYPE_HAL, Direct3DCreate9};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIFactory1,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
    Graphics::Gdi::{DEVMODEW, EnumDisplaySettingsW},
    System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard},
    System::Memory::{GlobalLock, GlobalUnlock},
    System::Threading::CreateMutexW,
    UI::Input::KeyboardAndMouse::{GetAsyncKeyState, MAPVK_VK_TO_VSC_EX, MapVirtualKeyW},
    UI::Shell::ShellExecuteW,
    UI::WindowsAndMessaging::SW_SHOWNORMAL,
};
use winreg::{
    RegKey, RegValue,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WRITE, RegType::REG_BINARY},
};

const REGISTRY_PATH: &str = r"Software\Bethesda Softworks\Morrowind";

/// Morrowind.exe carries no application manifest, so Windows registry
/// virtualization sends its writes to `REGISTRY_PATH` here instead and serves
/// its reads from here first. This GUI is 64-bit, and virtualization only ever
/// applies to 32-bit processes, so it has to walk that same view by hand:
/// installs whose machine key is missing (Steam) or read-only (the default
/// HKLM ACL) keep all of their display settings in this copy alone.
const VIRTUAL_STORE_PATH: &str = r"Software\Classes\VirtualStore\MACHINE\SOFTWARE\WOW6432Node\Bethesda Softworks\Morrowind";

pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire() -> Result<Option<Self>> {
        let name = wide("MGEguiMutex");
        // SAFETY: The security attributes pointer is null and the name is nul-terminated.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error()).context("create single-instance mutex");
        }
        // SAFETY: GetLastError has no preconditions.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: CreateMutexW returned a valid owned handle.
            unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(Self { handle }))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: The handle is owned by this guard and closed once.
        unsafe { CloseHandle(self.handle) };
    }
}

pub fn morrowind_is_running() -> bool {
    let system = System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()));
    system.processes_by_exact_name(OsStr::new("Morrowind.exe")).next().is_some()
}

/// Dedicated VRAM of the most capable display adapter, in bytes, or `None` if
/// none could be enumerated. Used only for an advisory memory-use estimate in
/// the generator, so this deliberately skips the identity tracking, PCI
/// cross-referencing, and live utilization polling a full GPU inventory would
/// need. One plausible total is enough.
pub fn gpu_dedicated_video_memory_bytes() -> Option<u64> {
    // SAFETY: CreateDXGIFactory1 has no preconditions; dxgi.dll ships with Windows.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    let mut best = None;
    for index in 0.. {
        // SAFETY: `factory` is a valid COM object; EnumAdapters1 is a normal call on it.
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(_) => break,
        };
        // SAFETY: `adapter` is a valid COM object; GetDesc1 is a normal call on it.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if DXGI_ADAPTER_FLAG(desc.Flags as i32).contains(DXGI_ADAPTER_FLAG_SOFTWARE) {
            continue;
        }
        let memory = desc.DedicatedVideoMemory as u64;
        if best.is_none_or(|current| memory > current) {
            best = Some(memory);
        }
    }
    best
}

/// Largest square texture dimension the default display adapter can create, in
/// texels, or `None` if it could not be queried. Queried through Direct3D 9,
/// the API the injected renderer actually uses at runtime (wrapped from D3D8),
/// rather than the GUI's own rendering backend, whose adapter and
/// requested feature limits do not necessarily match what the game's D3D9
/// device will report.
pub fn max_texture_dimension() -> Option<u32> {
    // SAFETY: Direct3DCreate9 has no preconditions; d3d9.dll ships with Windows.
    let d3d9 = unsafe { Direct3DCreate9(D3D_SDK_VERSION) }?;
    let mut caps = D3DCAPS9::default();
    // SAFETY: `d3d9` is a valid COM object; `caps` is writable D3DCAPS9 storage.
    unsafe { d3d9.GetDeviceCaps(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, &mut caps) }.ok()?;
    Some(caps.MaxTextureWidth.min(caps.MaxTextureHeight))
}

pub fn validate_root(root: &Path) -> Result<()> {
    let required_files = ["Morrowind.exe", "Morrowind.ini", "mgeHost64.exe", "d3d8.dll", "dinput8.dll"];
    let missing = required_files
        .into_iter()
        .filter(|path| !root.join(path).is_file())
        .chain((!root.join("MGE3").is_dir()).then_some("MGE3"))
        .chain((!root.join("Data Files").is_dir()).then_some("Data Files"))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        anyhow::bail!(
            "{}",
            t!(
                "startup.invalid_root",
                path = root.display().to_string(),
                missing = missing.join(", ")
            )
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
}

pub fn display_modes() -> Vec<DisplayMode> {
    let mut result = Vec::new();
    let mut index = 0;
    loop {
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: mode points to writable DEVMODEW storage with dmSize initialized.
        let found = unsafe { EnumDisplaySettingsW(std::ptr::null(), index, &mut mode) };
        if found == 0 {
            break;
        }
        if mode.dmPelsWidth >= 640 && mode.dmPelsHeight >= 480 && mode.dmBitsPerPel >= 32 {
            result.push(DisplayMode {
                width: mode.dmPelsWidth,
                height: mode.dmPelsHeight,
                refresh: mode.dmDisplayFrequency,
            });
        }
        index += 1;
    }
    result.sort();
    result.dedup();
    result
}

#[derive(Clone, Copy, Debug)]
pub struct RegistrySettings {
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    pub windowed: bool,
    pub adapter: u32,
}

impl Default for RegistrySettings {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            refresh: 0,
            windowed: true,
            adapter: 0,
        }
    }
}

impl RegistrySettings {
    pub fn load() -> Self {
        // Read in the order the game resolves values: a virtualized copy shadows
        // the machine key value by value, so a partial copy still falls through.
        let keys = [open_virtual_store(KEY_READ).ok().flatten(), open_machine_key(KEY_READ)]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let defaults = Self::default();
        Self {
            width: read_reg_u32(&keys, "Screen Width").unwrap_or(defaults.width),
            height: read_reg_u32(&keys, "Screen Height").unwrap_or(defaults.height),
            refresh: read_reg_u32(&keys, "Refresh Rate").unwrap_or(defaults.refresh),
            windowed: !read_reg_bool(&keys, "Fullscreen").unwrap_or(true),
            adapter: read_reg_u32(&keys, "Adapter").unwrap_or(defaults.adapter),
        }
    }

    pub fn save(&self, disable_mge: bool) -> Result<()> {
        let machine = open_machine_key(KEY_READ | KEY_WRITE);
        let virtual_store = match machine {
            // The machine key is writable, so only keep an existing virtualized
            // copy in step. Creating one where the game never made one would
            // shadow every later write to the machine key. A copy that exists
            // but will not open is a failure, not an absent copy: writing only
            // the machine key would leave the game reading stale values.
            Some(_) => open_virtual_store(KEY_READ | KEY_WRITE)
                .context("open the virtualized copy of the Morrowind display settings for writing")?,
            // Missing or elevation-only machine key: this is exactly the case
            // Windows virtualizes for the game, so write where the game reads.
            None => Some(
                RegKey::predef(HKEY_CURRENT_USER)
                    .create_subkey_with_flags(VIRTUAL_STORE_PATH, KEY_READ | KEY_WRITE)
                    .map(|(key, _)| key)
                    .context("open the Morrowind display settings for writing")?,
            ),
        };

        // Write the virtualized copy first. It is the one the game reads, so if
        // a write fails part-way the machine key is left untouched rather than
        // half of each.
        for key in [virtual_store, machine].into_iter().flatten() {
            key.set_value("Screen Width", &self.width)?;
            key.set_value("Screen Height", &self.height)?;
            key.set_value("Refresh Rate", &self.refresh)?;
            key.set_raw_value("Fullscreen", &reg_bool(!self.windowed))?;
            key.set_value("Adapter", &self.adapter)?;
            key.set_raw_value("Pixelshader", &reg_bool(disable_mge))?;
        }
        Ok(())
    }
}

/// Morrowind stores its boolean settings as single-byte `REG_BINARY` values,
/// never as `REG_DWORD`: `Fullscreen`, `Pixelshader`, `Stencil`, `Mipmap` and
/// friends are all one byte wide. Writing the wrong type makes the game fall
/// back to its built-in default, which for `Fullscreen` means it stays
/// fullscreen and then fails to match a windowed-only resolution against the
/// display's mode list ("Could not match desired fullscreen mode").
fn reg_bool(value: bool) -> RegValue<'static> {
    RegValue {
        bytes: vec![u8::from(value)].into(),
        vtype: REG_BINARY,
    }
}

/// Read one of those flags without assuming its type: any non-zero byte counts
/// as set, which reads a correct one-byte `REG_BINARY` and a `REG_DWORD` left
/// behind by an earlier build of this GUI identically.
fn read_reg_bool(keys: &[RegKey], name: &str) -> Option<bool> {
    let value = keys.iter().find_map(|key| key.get_raw_value(name).ok())?;
    Some(value.bytes.iter().any(|byte| *byte != 0))
}

/// First key that carries the value wins, matching how the game resolves a
/// virtualized copy against the machine key.
fn read_reg_u32(keys: &[RegKey], name: &str) -> Option<u32> {
    keys.iter().find_map(|key| key.get_value(name).ok())
}

/// The real machine key. `KEY_WOW64_32KEY` puts this 64-bit process on the
/// 32-bit view the game uses, under `WOW6432Node`.
fn open_machine_key(access: u32) -> Option<RegKey> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(REGISTRY_PATH, access | KEY_WOW64_32KEY)
        .ok()
}

/// The per-user copy Windows redirects the game's writes into. Nothing under
/// `HKEY_CURRENT_USER` is WOW64-redirected here, so the path is used as-is.
/// Only a missing copy reads as `None`; anything else is reported, because a
/// copy that exists and cannot be opened still shadows the machine key.
fn open_virtual_store(access: u32) -> io::Result<Option<RegKey>> {
    match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(VIRTUAL_STORE_PATH, access) {
        Ok(key) => Ok(Some(key)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub struct KeyCapture {
    previous: [bool; 256],
}

impl KeyCapture {
    pub fn begin() -> Self {
        let mut capture = Self { previous: [false; 256] };
        for vk in 1..256 {
            // SAFETY: All byte-sized virtual-key values are accepted.
            capture.previous[vk] = unsafe { GetAsyncKeyState(vk as i32) } < 0;
        }
        capture
    }

    pub fn poll(&mut self) -> Option<u8> {
        for vk in 1..256 {
            // SAFETY: All byte-sized virtual-key values are accepted.
            let down = unsafe { GetAsyncKeyState(vk as i32) } < 0;
            let pressed = down && !self.previous[vk];
            self.previous[vk] = down;
            if !pressed {
                continue;
            }
            // SAFETY: MapVirtualKeyW accepts the virtual-key value and mapping kind.
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX) };
            if scan == 0 {
                continue;
            }
            let mut direct_input = (scan & 0xFF) as u8;
            if ((scan >> 8) & 0xFF) == 0xE0 {
                direct_input |= 0x80;
            }
            return Some(direct_input);
        }
        None
    }
}

/// Current clipboard text, or `None` when the clipboard holds no Unicode text.
///
/// egui can *write* the clipboard (`Context::copy_text`) but offers no read: the
/// paste it understands arrives as an `Event::Paste` synthesized by the windowing
/// backend from a real Ctrl+V. A menu item has no such event behind it, so the
/// shader editor's **Edit → Paste** reads the clipboard itself.
pub fn clipboard_text() -> Option<String> {
    // SAFETY: Each call below is guarded on the previous one succeeding, and the
    // clipboard is closed on every path out. `GlobalLock` yields a pointer valid
    // until the matching `GlobalUnlock`, and the data is only read within it.
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 || OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        let text = if handle.is_null() {
            None
        } else {
            let pointer = GlobalLock(handle) as *const u16;
            if pointer.is_null() {
                None
            } else {
                let mut length = 0;
                while *pointer.add(length) != 0 {
                    length += 1;
                }
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
                GlobalUnlock(handle);
                Some(text)
            }
        };
        CloseClipboard();
        text
    }
}

/// `CF_UNICODETEXT`. `windows-sys` types the clipboard formats as `u32` but does
/// not export the constant under the enabled feature set.
const CF_UNICODETEXT: u32 = 13;

pub fn reveal_path(path: &Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("{} does not exist", path.display());
    }
    Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn()
        .with_context(|| format!("open Explorer for {}", path.display()))?;
    Ok(())
}

/// Open a file with its registered handler.
///
/// `ShellExecuteW` rather than the `Command::new` idiom [`reveal_path`] uses:
/// there is no executable to name for an arbitrary file type, and a
/// `cmd /C start` workaround flashes a console window. The return value is an
/// `HINSTANCE`-typed status where anything **above 32** means success.
pub fn open_with_shell(path: &Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("{} does not exist", path.display());
    }

    open_shell_target(path.as_os_str())
}

/// Open a URL with the user's registered handler.
pub fn open_url(url: &str) -> Result<()> {
    open_shell_target(OsStr::new(url))
}

fn open_shell_target(target: &OsStr) -> Result<()> {
    let target = target.to_string_lossy();
    let file = wide(&target);
    let operation = wide("open");
    // SAFETY: both strings are nul-terminated and outlive the call; a null
    // owner window and null parameters/directory are documented as valid.
    let status = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if status as usize <= 32 {
        anyhow::bail!("no application is registered to open {target}");
    }
    Ok(())
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
