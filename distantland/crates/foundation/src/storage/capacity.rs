//! Disk free-space preflight for writer sessions.
//!
//! Capacity checks are a writer-authority concern: they run before the first dirty write and are
//! unrelated to committed-state serialization or inventory validation.

use std::io;
use std::path::Path;

use anyhow::{Context, Result, ensure};

/// Ensures a preflight free-space budget can be satisfied.
pub(crate) fn ensure_free_space(dir: &Path, required_bytes: u64) -> Result<()> {
    let available = available_space(dir).with_context(|| format!("failed to query free space for {}", dir.display()))?;
    ensure!(
        available >= required_bytes,
        "insufficient free space for distant-land commit: need {required_bytes} bytes, have {available} bytes"
    );
    Ok(())
}

#[cfg(windows)]
fn available_space(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut available = 0_u64;
    let result = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(available)
    }
}

#[cfg(unix)]
fn available_space(path: &Path) -> io::Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let stats = unsafe { stats.assume_init() };
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn zero_required_bytes_succeeds_for_existing_dir() {
        let dir = tempdir().expect("tempdir");
        ensure_free_space(dir.path(), 0).expect("zero budget must pass when free-space query succeeds");
    }

    #[test]
    fn missing_dir_reports_query_failure() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        // Do not assert on free-space amounts; only that a bad path fails the query path.
        let error = ensure_free_space(&missing, 1).expect_err("missing path must fail free-space query");
        let message = format!("{error:#}");
        assert!(message.contains("failed to query free space"), "unexpected error: {message}");
    }
}
