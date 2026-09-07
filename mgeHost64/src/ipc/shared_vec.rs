use std::cmp::Ordering;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use bytemuck::Pod;
use tracing::error;
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};

use crate::abi::{AllocVecParameters, Handle64, MAX_WAIT, RenderMesh, VecId, VecShare};
use crate::error::HostError;
use crate::win::{
    MappedView, OwnedHandle, allocation_granularity, close_remote_handle, commit_pages, create_auto_reset_event,
    create_reserved_mapping, duplicate_to_process, map_view, set_event, wait_multiple,
};

#[derive(Clone, Copy, Debug)]
pub struct SharedVecLayout {
    pub header_bytes: u32,
    pub window_size: u32,
    pub window_bytes: u32,
    pub max_size: u32,
    pub reserved_bytes: u32,
    pub element_bytes: u32,
}

impl SharedVecLayout {
    pub fn from_request(max_elements: u32, window_elements: u32, element_bytes: u32) -> Self {
        let granularity = allocation_granularity();
        let mut window_bytes = element_bytes.wrapping_mul(window_elements.max(1));
        let bytes_over = window_bytes % granularity;
        if bytes_over > 0 {
            window_bytes += granularity - bytes_over;
        }
        let window_size = (window_bytes / element_bytes).max(1);

        let mut max_size = max_elements.max(1);
        let elements_over = max_size % window_size;
        if elements_over > 0 {
            max_size += window_size - elements_over;
        }

        let mut header_bytes = size_of::<VecShare>() as u32;
        let header_over = header_bytes % granularity;
        if header_over > 0 {
            header_bytes += granularity - header_over;
        }

        let reserved_bytes = max_size.wrapping_mul(window_bytes);
        Self {
            header_bytes,
            window_size,
            window_bytes,
            max_size,
            reserved_bytes,
            element_bytes,
        }
    }
}

struct RemoteHandleGuard {
    client_process: HANDLE,
    handle32: u32,
    armed: bool,
}

impl RemoteHandleGuard {
    fn new(client_process: HANDLE, handle32: HANDLE) -> Self {
        Self {
            client_process,
            handle32: handle32 as usize as u32,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RemoteHandleGuard {
    fn drop(&mut self) {
        if self.armed && self.handle32 != 0 {
            close_remote_handle(self.client_process, self.handle32);
        }
    }
}

struct RemoteHandles32 {
    shared_mem32: u32,
    update_event32: u32,
    complete_event32: u32,
}

fn duplicate_remote_handles_with<D>(
    client_process: HANDLE,
    shared_mem: HANDLE,
    update_event: HANDLE,
    complete_event: HANDLE,
    mut duplicate: D,
) -> Result<RemoteHandles32, HostError>
where
    D: FnMut(HANDLE, HANDLE) -> Result<HANDLE, HostError>,
{
    let shared_mem32 = duplicate(shared_mem, client_process)?;
    let mut shared_mem_guard = RemoteHandleGuard::new(client_process, shared_mem32);

    let update_event32 = duplicate(update_event, client_process)?;
    let mut update_event_guard = RemoteHandleGuard::new(client_process, update_event32);

    let complete_event32 = duplicate(complete_event, client_process)?;
    let mut complete_event_guard = RemoteHandleGuard::new(client_process, complete_event32);

    shared_mem_guard.disarm();
    update_event_guard.disarm();
    complete_event_guard.disarm();

    Ok(RemoteHandles32 {
        shared_mem32: shared_mem32 as usize as u32,
        update_event32: update_event32 as usize as u32,
        complete_event32: complete_event32 as usize as u32,
    })
}

fn wait_read_impl<W>(shared: &VecShare, handles: [HANDLE; 2], milliseconds: u32, mut wait: W) -> Result<(), HostError>
where
    W: FnMut(&[HANDLE], u32) -> Result<u32, HostError>,
{
    // Preserve the original size we observed when the read began; update notifications for
    // old batches are expected, and only a size change or completion signal should wake us.
    let old_size = shared.size();
    while shared.reading64() != 0 && old_size == shared.size() {
        match wait(&handles, milliseconds)? {
            WAIT_OBJECT_0 => {}
            value if value == WAIT_OBJECT_0 + 1 => {
                shared.set_reading64(0);
                return Ok(());
            }
            WAIT_TIMEOUT => return Ok(()),
            _ => return Err(HostError::listen("Unexpected wait result while reading shared vector")),
        }
    }
    Ok(())
}

pub struct SharedVec {
    pub id: VecId,
    pub layout: SharedVecLayout,
    _mapping: OwnedHandle,
    _header_view: MappedView,
    _buffer_view: MappedView,
    shared: NonNull<VecShare>,
    writing: bool,
}

impl SharedVec {
    /// Owns the local mapping views and the duplicated handles the 32-bit client uses
    /// for the same storage.
    #[allow(clippy::cast_ptr_alignment)]
    pub fn create(id: VecId, client_process: HANDLE, params: &mut AllocVecParameters) -> Result<Self, HostError> {
        let layout = SharedVecLayout::from_request(
            params.max_capacity_in_elements,
            params.window_size_in_elements,
            params.element_size,
        );
        let total_bytes = layout.header_bytes.wrapping_add(layout.reserved_bytes);
        let mapping = OwnedHandle(create_reserved_mapping(total_bytes)?);
        let header_ptr = map_view(mapping.raw(), 0, size_of::<VecShare>())?;
        let mut header_view = MappedView::new(header_ptr, size_of::<VecShare>())?;
        commit_pages(header_view.as_ptr(), size_of::<VecShare>())?;

        let shared =
            NonNull::new(header_view.as_bytes_mut().as_mut_ptr().cast::<VecShare>()).expect("header pointer is non-null");

        let buffer_ptr = map_view(mapping.raw(), layout.header_bytes, layout.reserved_bytes as usize)?;
        let buffer_view = MappedView::new(buffer_ptr, layout.reserved_bytes as usize)?;

        let update_event = OwnedHandle(create_auto_reset_event()?);
        let complete_event = OwnedHandle(create_auto_reset_event()?);
        let remote_handles = duplicate_remote_handles_with(
            client_process,
            mapping.raw(),
            update_event.raw(),
            complete_event.raw(),
            duplicate_to_process,
        )?;

        let initial_header = VecShare {
            client_process: Handle64::from_u64(client_process as usize as u64),
            shared_mem64: Handle64::from_u64(mapping.raw() as usize as u64),
            update_event64: Handle64::from_u64(update_event.raw() as usize as u64),
            complete_event64: Handle64::from_u64(complete_event.raw() as usize as u64),
            users64: AtomicU32::new(1),
            users32: AtomicU32::new(0),
            shared_mem32: remote_handles.shared_mem32,
            update_event32: remote_handles.update_event32,
            complete_event32: remote_handles.complete_event32,
            ..VecShare::default()
        };

        unsafe {
            // Safety: the mapped header view has been committed for at least size_of::<VecShare>()
            // bytes, so writing a freshly initialized header into that region is valid.
            shared.as_ptr().write(initial_header);
        }

        // Ownership of the raw handles moves into the shared header. They are later reclaimed
        // from the stored 64-bit values when the final owner drops the vector.
        std::mem::forget(update_event);
        std::mem::forget(complete_event);

        params.reserved_bytes = layout.reserved_bytes;
        params.window_bytes = layout.window_bytes;
        params.header_bytes = layout.header_bytes;
        params.shared_mem32 = remote_handles.shared_mem32;
        params.id = id;

        let mut result = Self {
            id,
            layout,
            _mapping: mapping,
            _header_view: header_view,
            _buffer_view: buffer_view,
            shared,
            writing: false,
        };
        if params.initial_capacity > 0 {
            result.reserve(params.initial_capacity)?;
        }
        Ok(result)
    }

    /// Returns the mapped shared header.
    fn shared(&self) -> &VecShare {
        unsafe { self.shared.as_ref() }
    }

    /// Computes the byte offset of `index` within the windowed data region.
    fn element_offset(&self, index: u32) -> usize {
        let window_index = index / self.layout.window_size;
        let element_index = index % self.layout.window_size;
        let window_offset = window_index * self.layout.window_bytes;
        window_offset as usize + (element_index * self.layout.element_bytes) as usize
    }

    /// Returns a read-only slice inside the mapped data region.
    fn buffer_range(&self, offset: usize, len: usize) -> &[u8] {
        let end = offset.checked_add(len).expect("buffer range overflow");
        self._buffer_view
            .as_bytes()
            .get(offset..end)
            .expect("buffer range must stay within the mapped shared vector")
    }

    /// Returns a mutable slice inside the mapped data region.
    fn buffer_range_mut(&mut self, offset: usize, len: usize) -> &mut [u8] {
        let end = offset.checked_add(len).expect("buffer range overflow");
        self._buffer_view
            .as_bytes_mut()
            .get_mut(offset..end)
            .expect("buffer range must stay within the mapped shared vector")
    }

    /// Returns the next uncommitted byte inside the mapped data region.
    fn committed_end_ptr(&self) -> Result<*mut c_void, HostError> {
        self._buffer_view.ptr_at(self.shared().committed_bytes() as usize)
    }

    /// Checks if this vector is formatted for a specific Plain Old Data (POD) type size.
    pub fn is_type<T: Pod>(&self) -> bool {
        self.layout.element_bytes == size_of::<T>() as u32
    }

    /// Checks whether this vector mapping is no longer required by the 32-bit client process and can be fully dropped.
    pub fn can_free(&self) -> bool {
        let shared = self.shared();
        shared.users32.load(AtomicOrdering::Acquire) == 0 && shared.users64.load(AtomicOrdering::Acquire) == 1
    }

    /// Returns the number of elements logically present in the vector.
    pub fn size(&self) -> u32 {
        self.shared().size()
    }

    /// Clears the logical contents while retaining committed pages for reuse.
    pub fn reset(&mut self) {
        self.shared().set_size(0);
    }

    /// Returns the number of elements currently backed by committed pages.
    pub fn capacity(&self) -> u32 {
        (self.shared().committed_bytes() / self.layout.window_bytes) * self.layout.window_size
    }

    /// Ensures the vector can hold at least `num_elements`.
    ///
    /// # Errors
    ///
    /// Returns an error when the request exceeds the negotiated maximum size or page commits fail.
    pub fn reserve(&mut self, num_elements: u32) -> Result<(), HostError> {
        let capacity = self.capacity();
        if capacity >= num_elements {
            return Ok(());
        }
        if num_elements > self.layout.max_size {
            return Err(HostError::init(format!(
                "Vector {} requested capacity beyond max size",
                self.id
            )));
        }
        let num_extra_windows = (num_elements - capacity).div_ceil(self.layout.window_size);
        let bytes_needed = num_extra_windows.saturating_mul(self.layout.window_bytes);
        let current_end = self.committed_end_ptr()?;
        commit_pages(current_end, bytes_needed as usize)?;
        self.shared().add_committed_bytes(bytes_needed);
        Ok(())
    }

    /// Commits one additional window of storage.
    fn extend(&mut self) -> Result<(), HostError> {
        let committed = self.shared().committed_bytes();
        if committed + self.layout.window_bytes > self.layout.reserved_bytes {
            error!(
                "Vector {} exceeded maximum reserved size of {} bytes ({} elements of {} max)",
                self.id,
                self.layout.reserved_bytes,
                self.size(),
                self.layout.max_size
            );
            return Err(HostError::init("Vector exceeded maximum reserved size"));
        }
        let current_end = self.committed_end_ptr()?;
        commit_pages(current_end, self.layout.window_bytes as usize)?;
        self.shared().add_committed_bytes(self.layout.window_bytes);
        Ok(())
    }

    /// Marks the beginning of a write batch.
    pub fn start_write(&mut self) {
        self.writing = true;
    }

    /// Signals that a partial write batch has become visible to the peer.
    ///
    /// # Errors
    ///
    /// Returns an error when the update event cannot be signaled.
    pub fn update_write(&self) -> Result<(), HostError> {
        let handle = self.shared().update_event64.to_u64() as HANDLE;
        set_event(handle)
    }

    /// Finishes the current write batch, if any.
    ///
    /// # Errors
    ///
    /// Returns an error when the completion event cannot be signaled.
    pub fn end_write(&mut self) -> Result<(), HostError> {
        if !self.writing {
            return Ok(());
        }
        self.writing = false;
        let handle = self.shared().complete_event64.to_u64() as HANDLE;
        set_event(handle)
    }

    /// Marks the start of a read phase and waits for pending writes to settle.
    ///
    /// # Errors
    ///
    /// Returns any wait error from the underlying synchronization primitives.
    pub fn start_read(&mut self) -> Result<(), HostError> {
        self.shared().set_reading64(1);
        self.wait_read(MAX_WAIT)
    }

    /// Waits for new readable data or completion from the peer.
    ///
    /// # Errors
    ///
    /// Returns any wait error from the underlying synchronization primitives.
    pub fn wait_read(&mut self, milliseconds: u32) -> Result<(), HostError> {
        let handles = [
            self.shared().update_event64.to_u64() as HANDLE,
            self.shared().complete_event64.to_u64() as HANDLE,
        ];
        wait_read_impl(self.shared(), handles, milliseconds, wait_multiple)
    }

    /// Marks the read phase as complete.
    pub fn end_read(&mut self) {
        self.shared().set_reading64(0);
    }

    /// Returns the element at `index`.
    ///
    /// # Panics
    ///
    /// Panics when `T` does not match the vector's negotiated element type or when `index`
    /// points outside the committed data.
    pub fn get<T: Pod>(&self, index: u32) -> T {
        assert!(self.is_type::<T>());
        bytemuck::pod_read_unaligned(self.buffer_range(self.element_offset(index), size_of::<T>()))
    }

    /// Overwrites the element at `index`.
    ///
    /// # Panics
    ///
    /// Panics when `T` does not match the vector's negotiated element type or when `index`
    /// points outside the committed data.
    pub fn set<T: Pod>(&mut self, index: u32, value: T) {
        assert!(self.is_type::<T>());
        self.buffer_range_mut(self.element_offset(index), size_of::<T>())
            .copy_from_slice(bytemuck::bytes_of(&value));
    }

    /// Appends one element, extending the committed region if needed.
    ///
    /// When a write batch is active, the method signals the update event whenever a newly
    /// committed window becomes visible so the 32-bit client can process completed chunks.
    ///
    /// # Errors
    ///
    /// Returns an error when the mapping cannot grow or an update event cannot be signaled.
    ///
    /// # Panics
    ///
    /// Panics when `T` does not match the vector's negotiated element type.
    pub fn push<T: Pod>(&mut self, value: T) -> Result<(), HostError> {
        assert!(self.is_type::<T>());
        let old_size = self.size();
        let new_size = old_size + 1;
        let capacity = self.capacity();
        if new_size > capacity {
            self.extend()?;
        }
        let sub_index = old_size % self.layout.window_size;
        self.buffer_range_mut(self.element_offset(old_size), size_of::<T>())
            .copy_from_slice(bytemuck::bytes_of(&value));
        self.shared().set_size(new_size);

        // Window boundaries are the only points where the consumer can safely assume that an
        // additional committed span has become readable without racing a later page commit.
        if self.writing && sub_index == 0 {
            self.update_write()?;
        }
        Ok(())
    }

    /// Iterates each readable element in logical order.
    ///
    /// The loop re-checks `size()` before and after waiting so producers can append data
    /// incrementally while the consumer drains the vector.
    ///
    /// # Errors
    ///
    /// Returns any wait error encountered while waiting for more data.
    ///
    /// # Panics
    ///
    /// Panics when `T` does not match the vector's negotiated element type.
    pub fn for_each<T: Pod>(&mut self, mut f: impl FnMut(T)) -> Result<(), HostError> {
        assert!(self.is_type::<T>());
        let mut index = 0;
        while index < self.size() {
            if index >= self.size() {
                self.wait_read(MAX_WAIT)?;
            }
            if index >= self.size() {
                break;
            }
            f(self.get::<T>(index));
            index += 1;
        }
        Ok(())
    }

    /// Copies the logical contents into a native `Vec`.
    ///
    /// # Errors
    ///
    /// Returns any wait error encountered while reading.
    #[cfg(test)]
    pub fn read_all<T: Pod>(&mut self) -> Result<Vec<T>, HostError> {
        let mut output = Vec::with_capacity(self.size() as usize);
        self.for_each(|value| output.push(value))?;
        Ok(output)
    }

    /// Sorts the stored `RenderMesh` elements in place using a scratch buffer.
    ///
    /// # Panics
    ///
    /// Panics if reading back the already-populated vector fails.
    pub fn sort_render_meshes(&mut self, scratch: &mut Vec<RenderMesh>, ordering: fn(&RenderMesh, &RenderMesh) -> Ordering) {
        scratch.clear();
        scratch.reserve(self.size() as usize);
        self.for_each(|mesh| scratch.push(mesh))
            .expect("reading existing render mesh data must succeed");
        scratch.sort_by(ordering);
        for (index, value) in scratch.iter().copied().enumerate() {
            self.set(index as u32, value);
        }
    }

    /// Creates a self-contained vector that uses the current process as both producer and consumer.
    #[cfg(test)]
    pub(crate) fn create_for_tests<T: Pod>(max_elements: u32, window_elements: u32) -> Result<Self, HostError> {
        let mut params = AllocVecParameters {
            max_capacity_in_elements: max_elements,
            window_size_in_elements: window_elements,
            element_size: size_of::<T>() as u32,
            initial_capacity: 0,
            ..AllocVecParameters::default()
        };
        Self::create(
            1,
            unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() },
            &mut params,
        )
    }
}

impl Drop for SharedVec {
    fn drop(&mut self) {
        let previous = self.shared().users64.fetch_sub(1, AtomicOrdering::AcqRel);
        if previous == 0 {
            self.shared().users64.store(0, AtomicOrdering::Release);
            return;
        }
        if previous > 1 || self.shared().users32.load(AtomicOrdering::Acquire) > 0 {
            return;
        }

        let shared = self.shared();
        // This is the last host-side owner and the client has already released its references,
        // so we can retire the duplicated 32-bit handles and then reclaim the native events.
        close_remote_handle(shared.client_process.to_u64() as HANDLE, shared.update_event32);
        close_remote_handle(shared.client_process.to_u64() as HANDLE, shared.complete_event32);
        close_remote_handle(shared.client_process.to_u64() as HANDLE, shared.shared_mem32);
        let update = shared.update_event64.to_u64() as HANDLE;
        let complete = shared.complete_event64.to_u64() as HANDLE;
        let _ = OwnedHandle(update);
        let _ = OwnedHandle(complete);
    }
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;
    use std::sync::atomic::{AtomicU8, AtomicU32};

    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;

    use super::*;
    use crate::win::{create_auto_reset_event, create_reserved_mapping};

    #[test]
    fn layout_rounding_matches_cpp_rules() {
        let layout = SharedVecLayout::from_request(200_000, 1, 84);
        assert_eq!(layout.window_bytes % allocation_granularity(), 0);
        assert_eq!(layout.header_bytes % allocation_granularity(), 0);
        assert_eq!(layout.max_size % layout.window_size, 0);
    }

    #[test]
    fn layout_wraps_like_cpp_u32_math() {
        let layout = SharedVecLayout::from_request(200_000, 1, 88);
        assert_eq!(layout.window_bytes, 65_536);
        assert_eq!(layout.window_size, 744);
        assert_eq!(layout.max_size, 200_136);
        assert_eq!(layout.reserved_bytes, 231_211_008);
        assert_eq!(layout.header_bytes.wrapping_add(layout.reserved_bytes), 231_276_544);
    }

    #[test]
    fn wait_read_update_returns_without_clearing_reading() {
        let shared = VecShare {
            size: AtomicU32::new(1),
            reading64: AtomicU8::new(1),
            ..VecShare::default()
        };
        let mut wait_calls = 0;
        wait_read_impl(&shared, [std::ptr::null_mut(); 2], MAX_WAIT, |_, _| {
            wait_calls += 1;
            shared.set_size(2);
            Ok(WAIT_OBJECT_0)
        })
        .unwrap();
        assert_eq!(wait_calls, 1);
        assert_eq!(shared.reading64(), 1);
        assert_eq!(shared.size(), 2);
    }

    #[test]
    fn wait_read_complete_clears_reading() {
        let shared = VecShare {
            size: AtomicU32::new(1),
            reading64: AtomicU8::new(1),
            ..VecShare::default()
        };
        wait_read_impl(&shared, [std::ptr::null_mut(); 2], MAX_WAIT, |_, _| Ok(WAIT_OBJECT_0 + 1)).unwrap();
        assert_eq!(shared.reading64(), 0);
    }

    #[test]
    fn wait_read_timeout_preserves_reading_state() {
        let shared = VecShare {
            size: AtomicU32::new(1),
            reading64: AtomicU8::new(1),
            ..VecShare::default()
        };
        wait_read_impl(&shared, [std::ptr::null_mut(); 2], MAX_WAIT, |_, _| Ok(WAIT_TIMEOUT)).unwrap();
        assert_eq!(shared.reading64(), 1);
    }

    #[test]
    fn push_only_signals_updates_during_a_write_phase() {
        let mut vec = SharedVec::create_for_tests::<u32>(8, 1).unwrap();
        let update_event = vec.shared().update_event64.to_u64() as HANDLE;

        vec.push(1_u32).unwrap();
        assert_eq!(wait_multiple(&[update_event], 0).unwrap(), WAIT_TIMEOUT);

        let mut vec = SharedVec::create_for_tests::<u32>(8, 1).unwrap();
        let update_event = vec.shared().update_event64.to_u64() as HANDLE;
        vec.start_write();
        vec.push(2_u32).unwrap();
        assert_eq!(wait_multiple(&[update_event], 0).unwrap(), WAIT_OBJECT_0);
    }

    #[test]
    fn end_write_only_signals_once() {
        let mut vec = SharedVec::create_for_tests::<u32>(8, 1).unwrap();
        let complete_event = vec.shared().complete_event64.to_u64() as HANDLE;

        vec.start_write();
        vec.end_write().unwrap();
        assert_eq!(wait_multiple(&[complete_event], 0).unwrap(), WAIT_OBJECT_0);

        vec.end_write().unwrap();
        assert_eq!(wait_multiple(&[complete_event], 0).unwrap(), WAIT_TIMEOUT);
    }

    #[test]
    fn get_set_push_and_read_all_preserve_order_across_windows() {
        let mut vec = SharedVec::create_for_tests::<u32>(8, 2).unwrap();
        vec.push(10).unwrap();
        vec.push(20).unwrap();
        vec.push(30).unwrap();
        vec.push(40).unwrap();
        vec.set(2, 35);

        assert_eq!(vec.get::<u32>(0), 10);
        assert_eq!(vec.get::<u32>(2), 35);
        assert_eq!(vec.read_all::<u32>().unwrap(), vec![10, 20, 35, 40]);
    }

    #[test]
    fn reserve_rejects_capacity_beyond_max_size() {
        let mut vec = SharedVec::create_for_tests::<u32>(4, 2).unwrap();
        assert!(vec.reserve(vec.layout.max_size + 1).is_err());
    }

    #[test]
    fn partial_remote_cleanup_closes_handles_on_second_duplicate_failure() {
        let mapping = OwnedHandle(create_reserved_mapping(allocation_granularity()).unwrap());
        let update_event = OwnedHandle(create_auto_reset_event().unwrap());
        let complete_event = OwnedHandle(create_auto_reset_event().unwrap());
        let client_process = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() };
        let mut duplicates = Vec::new();
        let mut attempts = 0;

        let result = duplicate_remote_handles_with(
            client_process,
            mapping.raw(),
            update_event.raw(),
            complete_event.raw(),
            |source, target| {
                attempts += 1;
                if attempts == 2 {
                    return Err(HostError::init("injected failure"));
                }
                let duplicate = duplicate_to_process(source, target)?;
                duplicates.push(duplicate);
                Ok(duplicate)
            },
        );

        assert!(result.is_err());
        assert_eq!(duplicates.len(), 1);
        assert!(map_view(duplicates[0], 0, size_of::<VecShare>()).is_err());
    }

    #[test]
    fn partial_remote_cleanup_closes_handles_on_third_duplicate_failure() {
        let mapping = OwnedHandle(create_reserved_mapping(allocation_granularity()).unwrap());
        let update_event = OwnedHandle(create_auto_reset_event().unwrap());
        let complete_event = OwnedHandle(create_auto_reset_event().unwrap());
        let client_process = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() };
        let mut duplicates = Vec::new();
        let mut attempts = 0;

        let result = duplicate_remote_handles_with(
            client_process,
            mapping.raw(),
            update_event.raw(),
            complete_event.raw(),
            |source, target| {
                attempts += 1;
                if attempts == 3 {
                    return Err(HostError::init("injected failure"));
                }
                let duplicate = duplicate_to_process(source, target)?;
                duplicates.push(duplicate);
                Ok(duplicate)
            },
        );

        assert!(result.is_err());
        assert_eq!(duplicates.len(), 2);
        assert!(map_view(duplicates[0], 0, size_of::<VecShare>()).is_err());
        assert!(set_event(duplicates[1]).is_err());
    }
}
