//! Durable byte and streaming writers for the publish path.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Artifact durability class. Determines whether the sync may be deferred to the pre-state barrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncClass {
    /// Bulk payloads (static shards, atlas pages, terrain.bin, terrain DDS).
    /// Durability is established at the pre-state sync barrier.
    Payload,
    /// Small artifacts (version, usage.data, occlusion). Synced at write time.
    SmallArtifact,
    /// `generation_state.bin` and its invalidation. Synced at write time.
    State,
}

impl SyncClass {
    /// Whether writes in this class buffer at write time and sync at the pre-state barrier.
    pub fn defers_sync(self) -> bool {
        matches!(self, SyncClass::Payload)
    }
}

/// Evidence returned by a completed durable write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurableWriteResult {
    /// Accepted byte length.
    pub byte_len: u64,
    /// BLAKE3 digest of the accepted bytes.
    pub hash: blake3::Hash,
}

/// A writable target that can make its current bytes durable.
///
/// `File` is the only production implementer; the genericity exists so tests can observe *whether*
/// a sync happened. `File::sync_all` takes `&self`, so a counting mock cannot impl it. Hence the
/// `&mut self` receiver here. Do not look for a second production implementer, and do not collapse
/// this to a concrete `File`: the sync-count assertions in `durable/tests.rs` have no substitute.
pub trait SyncWrite: Write {
    /// Flushes the target's current bytes to durable storage.
    fn sync_all(&mut self) -> io::Result<()>;
}

impl SyncWrite for File {
    fn sync_all(&mut self) -> io::Result<()> {
        File::sync_all(self)
    }
}

/// The outcome of finishing a [`DurableSink`], depending on its class's sync policy.
pub enum SinkFinish<W> {
    /// The target was synced; the bytes are durable.
    Synced(DurableWriteResult),
    /// The target was flushed only: the caller must carry `target` to the pre-state sync barrier.
    Deferred {
        /// Evidence for the buffered write; durability is not yet established.
        result: DurableWriteResult,
        /// The written handle awaiting its barrier sync.
        target: W,
    },
}

/// One buffered payload write awaiting the pre-state sync barrier.
///
/// Holds the written `File` handle rather than a path so the barrier syncs exactly the object that
/// accepted the bytes (reopening on Windows can fail spuriously under AV scanners).
#[derive(Debug)]
pub struct PendingDurable {
    path: PathBuf,
    file: File,
    byte_len: u64,
}

impl PendingDurable {
    /// Wraps one deferred write's handle and its accepted byte length.
    pub fn new(path: PathBuf, file: File, byte_len: u64) -> Self {
        Self { path, file, byte_len }
    }

    /// The destination path, for barrier error context.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Syncs the retained handle, returning the now-durable byte length.
    pub fn sync(mut self) -> io::Result<u64> {
        SyncWrite::sync_all(&mut self.file)?;
        Ok(self.byte_len)
    }
}

/// A buffered streaming sink that hashes accepted bytes and, on finish, either syncs immediately or
/// hands its flushed target to the pre-state sync barrier, per its [`SyncClass`] policy.
pub struct DurableSink<W: SyncWrite> {
    writer: BufWriter<W>,
    hasher: blake3::Hasher,
    byte_len: u64,
    sync_class: SyncClass,
}

impl DurableSink<File> {
    /// Creates a durable file sink.
    pub fn create(path: &Path, capacity: usize, sync_class: SyncClass) -> io::Result<Self> {
        Ok(Self::new(File::create(path)?, capacity, sync_class))
    }
}

impl<W: SyncWrite> DurableSink<W> {
    fn new(writer: W, capacity: usize, sync_class: SyncClass) -> Self {
        Self {
            writer: BufWriter::with_capacity(capacity, writer),
            hasher: blake3::Hasher::new(),
            byte_len: 0,
            sync_class,
        }
    }

    /// Flushes the target and completes per the class policy, returning length and digest.
    ///
    /// Immediate classes flush, then `sync_all`. Deferred classes flush and extract the written
    /// target: durability happens when the pre-state barrier syncs the returned handle.
    pub fn finish(mut self) -> io::Result<SinkFinish<W>> {
        self.writer.flush()?;
        let result = DurableWriteResult {
            byte_len: self.byte_len,
            hash: self.hasher.finalize(),
        };

        if self.sync_class.defers_sync() {
            // We flushed above, so a failure here is a genuine write failure: fail the run.
            let target = self.writer.into_inner().map_err(|error| error.into_error())?;
            return Ok(SinkFinish::Deferred { result, target });
        }

        self.writer.get_mut().sync_all()?;
        Ok(SinkFinish::Synced(result))
    }
}

impl<W: SyncWrite> Write for DurableSink<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.writer.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        self.byte_len += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// Rejects a deferring class on the immediate-only helper paths.
fn expect_synced(finish: SinkFinish<File>) -> io::Result<DurableWriteResult> {
    match finish {
        SinkFinish::Synced(result) => Ok(result),
        SinkFinish::Deferred { .. } => Err(io::Error::other(
            "deferred sync classes must be written through PublicationWrites so the barrier can sync them",
        )),
    }
}

/// Writes a complete payload durably and returns its length and hash.
///
/// # Errors
///
/// Fails for deferring classes: those writes must flow through `PublicationWrites` so their handles
/// reach the pre-state sync barrier.
pub fn write_durable(path: &Path, bytes: &[u8], sync_class: SyncClass) -> io::Result<DurableWriteResult> {
    let mut sink = DurableSink::create(path, 64 * 1024, sync_class)?;
    sink.write_all(bytes)?;
    expect_synced(sink.finish()?)
}

#[cfg(test)]
mod tests;
