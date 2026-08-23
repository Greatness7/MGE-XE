use super::*;

#[derive(Default)]
struct TestTarget {
    bytes: Vec<u8>,
    syncs: usize,
}

impl Write for TestTarget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SyncWrite for TestTarget {
    fn sync_all(&mut self) -> io::Result<()> {
        self.syncs += 1;
        Ok(())
    }
}

#[test]
fn durable_bytes_return_exact_length_and_hash() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("artifact.bin");
    let bytes = b"durable phase two bytes";
    let mut sink = DurableSink::create(&path, 64 * 1024, SyncClass::SmallArtifact).unwrap();
    sink.write_all(bytes).unwrap();
    let SinkFinish::Synced(result) = sink.finish().unwrap() else {
        panic!("small artifacts must sync at finish");
    };
    assert_eq!(result.byte_len, bytes.len() as u64);
    assert_eq!(result.hash, blake3::hash(bytes));
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn buffered_sink_is_write_compatible_and_hashes_the_stream() {
    let mut sink = DurableSink::new(TestTarget::default(), 4, SyncClass::SmallArtifact);
    sink.write_all(b"abcdefgh").unwrap();
    let SinkFinish::Synced(result) = sink.finish().unwrap() else {
        panic!("immediate class must sync at finish");
    };
    assert_eq!(result.byte_len, 8);
    assert_eq!(result.hash, blake3::hash(b"abcdefgh"));
}

#[test]
fn only_the_payload_class_defers_its_sync() {
    assert!(SyncClass::Payload.defers_sync());
    assert!(!SyncClass::SmallArtifact.defers_sync());
    assert!(!SyncClass::State.defers_sync());
}

#[test]
fn expect_synced_rejects_a_deferring_class() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("payload.bin");
    let mut sink = DurableSink::create(&path, 64, SyncClass::Payload).unwrap();
    sink.write_all(b"bytes").unwrap();
    let error = expect_synced(sink.finish().unwrap()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
}

#[test]
fn deferred_finish_flushes_without_syncing() {
    let mut sink = DurableSink::new(TestTarget::default(), 4, SyncClass::Payload);
    sink.write_all(b"deferred payload").unwrap();
    let SinkFinish::Deferred { result, target } = sink.finish().unwrap() else {
        panic!("payload class must defer its sync");
    };
    assert_eq!(result.byte_len, 16);
    assert_eq!(result.hash, blake3::hash(b"deferred payload"));
    assert_eq!(target.bytes, b"deferred payload", "flush must land every buffered byte");
    assert_eq!(target.syncs, 0, "deferred finish must not sync");
}

#[test]
fn pending_durable_barrier_sync_makes_the_payload_durable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("payload.bin");
    let mut sink = DurableSink::create(&path, 64, SyncClass::Payload).unwrap();
    sink.write_all(b"barrier bytes").unwrap();
    let SinkFinish::Deferred { result, target } = sink.finish().unwrap() else {
        panic!("payload class must defer its sync");
    };
    let pending = PendingDurable::new(path.clone(), target, result.byte_len);
    assert_eq!(pending.path(), path);

    assert_eq!(pending.sync().unwrap(), 13);
    assert_eq!(std::fs::read(&path).unwrap(), b"barrier bytes");
}
