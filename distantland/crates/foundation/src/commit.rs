//! Authoritative-write evidence for the publish phase.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::storage::durable::{DurableSink, PendingDurable, SinkFinish, SyncClass};

/// Length and content identity captured by one durable artifact write.
#[derive(Clone, Debug)]
pub struct WrittenFile {
    pub path: PathBuf,
    pub byte_length: u64,
    pub content_blake3: [u8; 32],
}

/// Streaming writer that becomes durable on [`Self::finish`] or, for deferring classes, at the
/// pre-state sync barrier.
pub struct DurableFile {
    path: PathBuf,
    sink: DurableSink<File>,
    writes: PublicationWrites,
}

impl DurableFile {
    /// Finishes the file per its class policy, recording its inventory evidence.
    ///
    /// Immediate classes flush and sync here. Deferring classes flush only and register their
    /// handle for the pre-state sync barrier.
    pub fn finish(self) -> io::Result<WrittenFile> {
        self.writes.finish_sink(self.path, self.sink)
    }
}

impl Write for DurableFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.sink.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

/// The authoritative artifact writes performed by one publication, plus the deferred handles those
/// writes still owe the pre-state sync barrier.
///
/// Every authoritative write flows through here, so the ledger is complete by construction. The
/// publisher drains both: the barrier syncs the handles, and `WriterSession::finish_publish` then
/// checks each recorded write against the inventory it is about to publish. Clones share one
/// registry so writer threads record into the registry the publisher drains.
#[derive(Clone, Debug, Default)]
pub struct PublicationWrites {
    pending_durables: Arc<Mutex<Vec<PendingDurable>>>,
    written: Arc<Mutex<Vec<WrittenFile>>>,
}

impl PublicationWrites {
    /// Creates an empty ledger for one publication.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drains every deferred payload write registered by this ledger's clones.
    ///
    /// The caller must hand the result to the pre-state sync barrier; the state write must not
    /// happen while any drained entry remains unsynced.
    pub fn take_pending_durables(&self) -> Vec<PendingDurable> {
        std::mem::take(&mut *self.pending_durables.lock().expect("publication-writes lock poisoned"))
    }

    /// Drains the evidence of every authoritative artifact this run wrote.
    pub fn take_written(&self) -> Vec<WrittenFile> {
        std::mem::take(&mut *self.written.lock().expect("publication-writes lock poisoned"))
    }

    /// Whether no deferred payload write is awaiting the barrier (no-op path assertion).
    pub fn pending_durables_is_empty(&self) -> bool {
        self.pending_durables
            .lock()
            .expect("publication-writes lock poisoned")
            .is_empty()
    }

    /// Writes and hashes a complete authoritative artifact.
    ///
    /// Immediate classes are synced before returning. Deferring classes are flushed only and their
    /// handle is registered for the pre-state sync barrier.
    pub fn write_durable(&self, path: &Path, bytes: impl AsRef<[u8]>, sync_class: SyncClass) -> io::Result<WrittenFile> {
        let mut sink = DurableSink::create(path, 64 * 1024, sync_class)?;
        sink.write_all(bytes.as_ref())?;
        self.finish_sink(path.to_path_buf(), sink)
    }

    /// Opens a streaming writer for one authoritative artifact.
    pub fn create_durable(&self, path: &Path, sync_class: SyncClass) -> io::Result<DurableFile> {
        Ok(DurableFile {
            path: path.to_path_buf(),
            sink: DurableSink::create(path, 64 * 1024, sync_class)?,
            writes: self.clone(),
        })
    }

    /// Completes one sink, registering a deferred handle for the barrier and recording evidence.
    ///
    /// Immediate classes record only after their own successful sync. Deferred classes record at
    /// flush because `finish_publish` runs the barrier and returns on any sync failure before
    /// it reads this evidence or publishes state.
    fn finish_sink(&self, path: PathBuf, sink: DurableSink<File>) -> io::Result<WrittenFile> {
        let result = match sink.finish()? {
            SinkFinish::Synced(result) => result,
            SinkFinish::Deferred { result, target } => {
                self.pending_durables
                    .lock()
                    .expect("publication-writes lock poisoned")
                    .push(PendingDurable::new(path.clone(), target, result.byte_len));
                result
            }
        };
        let written = WrittenFile {
            path,
            byte_length: result.byte_len,
            content_blake3: *result.hash.as_bytes(),
        };
        self.written
            .lock()
            .expect("publication-writes lock poisoned")
            .push(written.clone());
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn deferred_payload_writes_share_one_registry_across_clones_and_drain_once() {
        let temp = tempfile::tempdir().unwrap();
        let shard = temp.path().join("shard.bin");
        let page = temp.path().join("page.dds");
        let usage = temp.path().join("usage.data");
        let writes = PublicationWrites::new();

        // A clone (as handed to a writer thread) registers into the same registry.
        let clone = writes.clone();
        clone.write_durable(&shard, b"shard bytes", SyncClass::Payload).unwrap();
        writes.write_durable(&page, b"page bytes", SyncClass::Payload).unwrap();

        // Immediate classes never enter the deferred registry.
        let small = writes
            .write_durable(&usage, b"usage bytes", SyncClass::SmallArtifact)
            .unwrap();
        assert_eq!(small.byte_length, 11);

        assert!(!writes.pending_durables_is_empty());
        let pending = writes.take_pending_durables();
        assert_eq!(pending.len(), 2);
        assert!(writes.pending_durables_is_empty());
        assert!(writes.take_pending_durables().is_empty(), "the registry drains exactly once");

        for entry in pending {
            entry.sync().unwrap();
        }
        assert_eq!(fs::read(&shard).unwrap(), b"shard bytes");
        assert_eq!(fs::read(&page).unwrap(), b"page bytes");
    }

    #[test]
    fn every_authoritative_write_is_recorded_as_evidence_once() {
        let temp = tempfile::tempdir().unwrap();
        let shard = temp.path().join("shard.bin");
        let usage = temp.path().join("usage.data");
        let writes = PublicationWrites::new();

        writes.write_durable(&shard, b"shard bytes", SyncClass::Payload).unwrap();
        let mut streamed = writes.create_durable(&usage, SyncClass::SmallArtifact).unwrap();
        streamed.write_all(b"usage bytes").unwrap();
        streamed.finish().unwrap();

        let mut evidence = writes.take_written();
        evidence.sort_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0].path, shard);
        assert_eq!(evidence[0].content_blake3, *blake3::hash(b"shard bytes").as_bytes());
        assert_eq!(evidence[1].path, usage);
        assert_eq!(evidence[1].byte_length, 11);
        assert!(writes.take_written().is_empty(), "evidence drains exactly once");
    }
}
