//! Cross-platform shared/exclusive storage lock guards.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// The stable lock-file location and typed acquisition entry point.
#[derive(Clone, Debug)]
pub struct LockFile {
    path: PathBuf,
}

impl LockFile {
    /// Creates a lock-file handle for `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the stable lock-file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Attempts one non-blocking exclusive acquisition for lock-behavior tests.
    #[cfg(test)]
    pub fn try_exclusive(&self) -> io::Result<Option<ExclusiveLockGuard>> {
        self.try_acquire(LockMode::Exclusive)
            .map(|guard| guard.map(|inner| ExclusiveLockGuard { _inner: inner }))
    }

    /// Attempts one non-blocking shared acquisition.
    pub fn try_shared(&self) -> io::Result<Option<SharedLockGuard>> {
        self.try_acquire(LockMode::Shared)
            .map(|guard| guard.map(|inner| SharedLockGuard { _inner: inner }))
    }

    /// Waits for at most `timeout` for an exclusive writer guard.
    pub fn exclusive_for(&self, timeout: Duration) -> io::Result<Option<ExclusiveLockGuard>> {
        self.acquire_for(LockMode::Exclusive, timeout)
            .map(|guard| guard.map(|inner| ExclusiveLockGuard { _inner: inner }))
    }

    /// Waits for at most `timeout` for a shared snapshot/session guard.
    pub fn shared_for(&self, timeout: Duration) -> io::Result<Option<SharedLockGuard>> {
        self.acquire_for(LockMode::Shared, timeout)
            .map(|guard| guard.map(|inner| SharedLockGuard { _inner: inner }))
    }

    fn try_acquire(&self, mode: LockMode) -> io::Result<Option<OsLockGuard>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)?;
        if os::try_lock(&file, mode)? {
            Ok(Some(OsLockGuard { file }))
        } else {
            Ok(None)
        }
    }

    fn acquire_for(&self, mode: LockMode, timeout: Duration) -> io::Result<Option<OsLockGuard>> {
        let started = Instant::now();
        loop {
            if let Some(guard) = self.try_acquire(mode)? {
                return Ok(Some(guard));
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(None);
            }
            thread::sleep((timeout - elapsed).min(Duration::from_millis(10)));
        }
    }
}

/// Exclusive writer ownership token; releases the OS lock on drop.
pub struct ExclusiveLockGuard {
    // Owns the lock guard so its `Drop` implementation releases the exclusive OS lock.
    _inner: OsLockGuard,
}

impl ExclusiveLockGuard {
    /// Exposes the held lock file for lock-behavior tests.
    #[cfg(test)]
    pub fn as_file(&self) -> &File {
        &self._inner.file
    }
}

/// Shared snapshot/session ownership token.
pub struct SharedLockGuard {
    // Owns the lock guard so its `Drop` implementation releases the shared OS lock.
    _inner: OsLockGuard,
}

impl SharedLockGuard {
    /// Exposes the held lock file for lock-behavior tests.
    #[cfg(test)]
    pub fn as_file(&self) -> &File {
        &self._inner.file
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LockMode {
    Shared,
    Exclusive,
}

struct OsLockGuard {
    file: File,
}

impl Drop for OsLockGuard {
    fn drop(&mut self) {
        os::unlock(&self.file);
    }
}

#[cfg(windows)]
mod os {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    use super::LockMode;

    pub(super) fn try_lock(file: &File, mode: LockMode) -> io::Result<bool> {
        let mut overlapped: OVERLAPPED = unsafe {
            // A zero offset and event are required for this synchronous byte-range lock.
            std::mem::zeroed()
        };
        let mut flags = LOCKFILE_FAIL_IMMEDIATELY;
        if mode == LockMode::Exclusive {
            flags |= LOCKFILE_EXCLUSIVE_LOCK;
        }
        let result = unsafe {
            // The handle remains owned by the guard for the full lock lifetime, the OVERLAPPED
            // value is live for this synchronous call, and the range deliberately covers the
            // entire possible file.
            LockFileEx(file.as_raw_handle(), flags, 0, u32::MAX, u32::MAX, &mut overlapped)
        };
        if result != 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) || error.kind() == io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub(super) fn unlock(file: &File) {
        let mut overlapped: OVERLAPPED = unsafe {
            // UnlockFileEx uses the same zero-offset range representation as acquisition.
            std::mem::zeroed()
        };
        unsafe {
            // The handle is valid until this guard's Drop completes. Unlock failure cannot be
            // reported from Drop; closing the handle immediately afterwards also releases it.
            UnlockFileEx(file.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut overlapped);
        }
    }
}

#[cfg(unix)]
mod os {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    use super::LockMode;

    pub(super) fn try_lock(file: &File, mode: LockMode) -> io::Result<bool> {
        let operation = match mode {
            LockMode::Shared => libc::LOCK_SH,
            LockMode::Exclusive => libc::LOCK_EX,
        } | libc::LOCK_NB;
        let result = unsafe {
            // The descriptor remains open in the returned guard and `operation` is one of the
            // platform flock constants.
            libc::flock(file.as_raw_fd(), operation)
        };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EAGAIN || code == libc::EWOULDBLOCK)
        {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub(super) fn unlock(file: &File) {
        unsafe {
            // The descriptor is valid until this guard's Drop completes. Closing it immediately
            // after this call also releases the flock if explicit unlock fails.
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(not(any(windows, unix)))]
compile_error!("the storage lock requires LockFileEx on Windows or flock on Unix");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_and_exclusive_guards_enforce_the_ownership_model() {
        let temp = tempfile::tempdir().unwrap();
        let lock = LockFile::new(temp.path().join(".writer.lock"));

        let shared_a = lock.try_shared().unwrap().unwrap();
        let shared_b = lock.try_shared().unwrap().unwrap();
        assert!(lock.try_exclusive().unwrap().is_none());
        assert!(shared_a.as_file().metadata().is_ok());
        drop(shared_a);
        drop(shared_b);

        let exclusive = lock.try_exclusive().unwrap().unwrap();
        assert!(exclusive.as_file().metadata().is_ok());
        assert!(lock.try_shared().unwrap().is_none());
        drop(exclusive);
        assert!(lock.try_shared().unwrap().is_some());
    }

    #[test]
    fn timed_acquisition_returns_none_at_the_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let lock = LockFile::new(temp.path().join(".writer.lock"));
        let _shared = lock.try_shared().unwrap().unwrap();
        assert!(lock.exclusive_for(Duration::from_millis(20)).unwrap().is_none());
    }
}
