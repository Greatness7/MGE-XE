use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use arc_swap::ArcSwapOption;
use tracing::error;

use crate::abi::D3dxVector3;
use crate::state::horizon::{HorizonParams, HorizonTable, TerrainHeightField};

/// Off-thread horizon-table build request.
pub(super) struct BuildRequest {
    pub(super) field: Arc<TerrainHeightField>,
    pub(super) eye: D3dxVector3,
    pub(super) params: HorizonParams,
    /// Field/parameter epoch checked when the result is picked up.
    pub(super) generation: u64,
    /// Eye-request epoch checked when the result is picked up.
    pub(super) request_id: u64,
}

/// Finished table published by the worker.
pub(super) struct BuiltTable {
    pub(super) table: Arc<HorizonTable>,
    pub(super) generation: u64,
    pub(super) request_id: u64,
}

/// Latest-wins request mailbox for the builder worker.
struct Mailbox {
    slot: Mutex<MailboxSlot>,
    signal: Condvar,
}

#[derive(Default)]
struct MailboxSlot {
    /// Newest requested build; overwritten (coalesced) when the eye moves again before the worker
    /// picks it up.
    request: Option<BuildRequest>,
    shutdown: bool,
}

/// Async builder with a latest-wins mailbox and lock-free result slot.
pub(super) struct HorizonBuilder {
    mailbox: Arc<Mailbox>,
    result: Arc<ArcSwapOption<BuiltTable>>,
    handle: Option<JoinHandle<()>>,
}

impl HorizonBuilder {
    /// Spawns the worker, or returns `None` so callers use synchronous builds.
    pub(super) fn spawn() -> Option<Self> {
        let mailbox = Arc::new(Mailbox {
            slot: Mutex::new(MailboxSlot::default()),
            signal: Condvar::new(),
        });
        let result = Arc::new(ArcSwapOption::<BuiltTable>::empty());
        let worker_mailbox = Arc::clone(&mailbox);
        let worker_result = Arc::clone(&result);
        match thread::Builder::new()
            .name("horizon-builder".to_string())
            .spawn(move || horizon_worker(worker_mailbox, worker_result))
        {
            Ok(handle) => Some(Self {
                mailbox,
                result,
                handle: Some(handle),
            }),
            Err(spawn_error) => {
                error!("Failed to spawn horizon builder thread ({spawn_error}); using synchronous builds");
                None
            }
        }
    }

    pub(super) fn post(&self, request: BuildRequest) {
        {
            let mut slot = self.mailbox.slot.lock().unwrap_or_else(PoisonError::into_inner);
            slot.request = Some(request);
        }
        self.mailbox.signal.notify_one();
    }

    pub(super) fn take_result(&self) -> Option<Arc<BuiltTable>> {
        self.result.swap(None)
    }

    #[cfg(test)]
    pub(super) fn has_result(&self) -> bool {
        self.result.load().is_some()
    }

    /// Test builder whose result timing is controlled by the test.
    #[cfg(test)]
    pub(super) fn stalled_for_tests() -> Self {
        Self {
            mailbox: Arc::new(Mailbox {
                slot: Mutex::new(MailboxSlot::default()),
                signal: Condvar::new(),
            }),
            result: Arc::new(ArcSwapOption::<BuiltTable>::empty()),
            handle: None,
        }
    }

    #[cfg(test)]
    pub(super) fn run_worker_once(&self) {
        let request = {
            let mut slot = self.mailbox.slot.lock().unwrap_or_else(PoisonError::into_inner);
            slot.request.take()
        };
        if let Some(request) = request {
            let table = HorizonTable::build(&request.field, request.eye, request.params);
            self.publish_result_for_tests(BuiltTable {
                table: Arc::new(table),
                generation: request.generation,
                request_id: request.request_id,
            });
        }
    }

    #[cfg(test)]
    pub(super) fn publish_result_for_tests(&self, result: BuiltTable) {
        self.result.store(Some(Arc::new(result)));
    }
}

impl Drop for HorizonBuilder {
    fn drop(&mut self) {
        {
            let mut slot = self.mailbox.slot.lock().unwrap_or_else(PoisonError::into_inner);
            slot.shutdown = true;
        }
        self.mailbox.signal.notify_one();
        if let Some(handle) = self.handle.take() {
            // A join error must not panic the dropper.
            let _ = handle.join();
        }
    }
}

/// Waits for the newest request, builds its table, and publishes it.
fn horizon_worker(mailbox: Arc<Mailbox>, result: Arc<ArcSwapOption<BuiltTable>>) {
    loop {
        let request = {
            let mut slot = mailbox.slot.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if slot.shutdown {
                    return;
                }
                if let Some(request) = slot.request.take() {
                    break request;
                }
                slot = mailbox.signal.wait(slot).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let table = HorizonTable::build(&request.field, request.eye, request.params);
        result.store(Some(Arc::new(BuiltTable {
            table: Arc::new(table),
            generation: request.generation,
            request_id: request.request_id,
        })));
    }
}
