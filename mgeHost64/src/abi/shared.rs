use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// Split 64-bit handle representation that matches the legacy packed ABI.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Zeroable, bytemuck::Pod)]
pub struct Handle64 {
    pub low: u32,
    pub high: u32,
}

impl Handle64 {
    pub fn from_u64(value: u64) -> Self {
        Self {
            low: value as u32,
            high: (value >> 32) as u32,
        }
    }

    pub fn to_u64(self) -> u64 {
        (self.low as u64) | ((self.high as u64) << 32)
    }
}

/// Header shared between the 32-bit client and the 64-bit host for each `SharedVec`.
#[repr(C)]
#[derive(Debug, Default)]
pub struct VecShare {
    /// Logical element count currently written into the vector.
    pub size: AtomicU32,
    /// Number of bytes currently committed in the backing mapping.
    pub committed_bytes: AtomicU32,
    /// Native handle of the client process, split for ABI compatibility.
    pub client_process: Handle64,
    /// Native handle of the file mapping in the 64-bit host.
    pub shared_mem64: Handle64,
    /// Host-side update event used for producer notifications.
    pub update_event64: Handle64,
    /// Host-side completion event used to finish a write batch.
    pub complete_event64: Handle64,
    /// Number of 64-bit holders currently keeping the vector alive.
    pub users64: AtomicU32,
    /// Number of 32-bit holders currently keeping the vector alive.
    pub users32: AtomicU32,
    /// Duplicated file-mapping handle value as seen by the 32-bit client.
    pub shared_mem32: u32,
    /// Duplicated update-event handle value as seen by the 32-bit client.
    pub update_event32: u32,
    /// Duplicated completion-event handle value as seen by the 32-bit client.
    pub complete_event32: u32,
    /// Set by the 32-bit side while it is actively reading.
    pub reading32: AtomicU8,
    /// Set by the 64-bit side while it is actively reading.
    pub reading64: AtomicU8,
    /// Reserved to keep the header layout compatible with the native code.
    pub _padding0: [u8; 2],
}

impl VecShare {
    /// Reads the logical element count through an atomic load because this header is shared with
    /// the 32-bit client process.
    pub fn size(&self) -> u32 {
        self.size.load(Ordering::Acquire)
    }

    /// Publishes the logical element count to the peer process.
    pub fn set_size(&self, size: u32) {
        self.size.store(size, Ordering::Release);
    }

    /// Reads the number of committed data bytes through an atomic load.
    pub fn committed_bytes(&self) -> u32 {
        self.committed_bytes.load(Ordering::Acquire)
    }

    /// Adds newly committed data bytes and publishes the result to the peer process.
    pub fn add_committed_bytes(&self, bytes: u32) {
        self.committed_bytes.fetch_add(bytes, Ordering::AcqRel);
    }

    /// Reads the 64-bit reader flag.
    pub fn reading64(&self) -> u8 {
        self.reading64.load(Ordering::Acquire)
    }

    /// Publishes the 64-bit reader flag.
    pub fn set_reading64(&self, reading: u8) {
        self.reading64.store(reading, Ordering::Release);
    }
}
