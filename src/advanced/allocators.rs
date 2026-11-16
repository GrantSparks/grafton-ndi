//! Application-supplied memory for received frames.
//!
//! The SDK calls an allocator and its matching free function on its own
//! threads, with an opaque value chosen at registration. Here that value is a
//! token naming one registered policy (a "generation"), never a pointer, and
//! every allocation is recorded as a lease under the token that made it. A
//! lease, and through it the policy, lives until the SDK frees that
//! allocation, however long after the policy was replaced that happens.

use std::{
    alloc::{alloc_zeroed, dealloc, Layout},
    collections::HashMap,
    mem::ManuallyDrop,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::NonNull,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard, OnceLock,
    },
};

use super::{
    invalid_input, PacketAttached, PacketItem, PacketItemRef, PacketReceiver, ReceiverControls,
};
#[cfg(any(test, ndi_has_advanced_receive_allocators))]
use super::{AudioCodec, VideoCodec};
#[cfg(any(test, ndi_has_advanced_receive_allocators))]
use crate::sys::bindings::*;
#[cfg(any(test, ndi_has_advanced_receive_allocators))]
use crate::AudioFormat;
use crate::{
    frames::{ValidatedAudioLayout, ValidatedVideoLayout},
    receiver::ReceiverCore,
    runtime::ProcessLifetimeRuntime,
    sys::advanced::ReceiveAllocators,
    FourCc, PixelFormat, RawCapture, Receiver, ReceiverMode, Rejected, Result,
};
#[cfg(ndi_has_advanced_receive_allocators)]
use std::ffi::c_void;

const MIB: usize = 1024 * 1024;

const MAX_ALLOCATION: usize = 256 * MIB;

/// Process-wide limits on custom receive allocation.
///
/// The limits cover every receiver in the process, including allocations made
/// by policies that have since been replaced and buffers whose release is
/// still in progress. Memory a policy allocates for its own bookkeeping is not
/// counted. A request the limits do not admit is declined, and the SDK then
/// drops that frame. [`AllocatorService::usage`] reports these declines through
/// [`AllocatorUsage::declined`] and [`AllocatorUsage::declined_allocations`].
///
/// The defaults are 256 generations, 1024 allocations, 1 GiB in total, 16
/// concurrent calls and 64 MiB per allocation, which holds an 8-bit 4:2:2
/// frame of 8K video.
///
/// # How the limits combine
///
/// A policy may return a larger buffer than the frame needs, so each call is
/// granted its buffer before the policy runs: the largest single allocation,
/// or what is left of the byte budget when that is less. A call is declined
/// only when its grant cannot hold the frame, and it counts as an allocation
/// while it runs. Limits are therefore accepted only when the stated number of
/// concurrent calls can each be granted a largest allocation:
/// `max_concurrent_calls` may not exceed `max_allocations`, and
/// `max_concurrent_calls * max_allocation_bytes` may not exceed `max_bytes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AllocatorLimits {
    max_generations: usize,
    max_allocations: usize,
    max_bytes: usize,
    max_concurrent_calls: usize,
    max_allocation_bytes: usize,
}

impl Default for AllocatorLimits {
    fn default() -> Self {
        Self::new()
    }
}

impl AllocatorLimits {
    /// Requested maximum number of live policy generations.
    #[must_use]
    pub fn configured_max_generations(&self) -> usize {
        self.max_generations
    }

    /// Requested maximum allocation count, including policy calls in progress.
    #[must_use]
    pub fn configured_max_allocations(&self) -> usize {
        self.max_allocations
    }

    /// Requested maximum retained and granted bytes in total.
    #[must_use]
    pub fn configured_max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Requested maximum number of concurrent policy calls.
    #[must_use]
    pub fn configured_max_concurrent_calls(&self) -> usize {
        self.max_concurrent_calls
    }

    /// Requested maximum bytes granted to one allocation.
    #[must_use]
    pub fn configured_max_allocation_bytes(&self) -> usize {
        self.max_allocation_bytes
    }

    /// The default limits: 256 generations, 1024 allocations, 1 GiB in total,
    /// 16 concurrent calls and 64 MiB per allocation.
    pub fn new() -> Self {
        Self {
            max_generations: 256,
            max_allocations: 1024,
            max_bytes: 1024 * MIB,
            max_concurrent_calls: 16,
            max_allocation_bytes: 64 * MIB,
        }
    }

    /// The number of policies that may exist at once. A replaced policy
    /// counts until the SDK has freed its last allocation.
    #[must_use]
    pub fn max_generations(mut self, value: usize) -> Self {
        self.max_generations = value;
        self
    }

    /// The number of allocations the SDK may hold at once, counting the
    /// policy calls in progress.
    #[must_use]
    pub fn max_allocations(mut self, value: usize) -> Self {
        self.max_allocations = value;
        self
    }

    /// The total bytes the SDK may hold at once, counting what the policy
    /// calls in progress have been granted.
    #[must_use]
    pub fn max_bytes(mut self, value: usize) -> Self {
        self.max_bytes = value;
        self
    }

    /// The number of policy calls that may run at the same time.
    #[must_use]
    pub fn max_concurrent_calls(mut self, value: usize) -> Self {
        self.max_concurrent_calls = value;
        self
    }

    /// The largest single allocation, at most 256 MiB.
    #[must_use]
    pub fn max_allocation_bytes(mut self, value: usize) -> Self {
        self.max_allocation_bytes = value;
        self
    }

    fn validate(self) -> Result<Self> {
        let limits_error = |cause| crate::Error::InvalidInput(cause);
        if self.max_generations == 0
            || self.max_allocations == 0
            || self.max_concurrent_calls == 0
            || self.max_allocation_bytes == 0
        {
            return Err(limits_error(crate::InputDefect::AllocatorPositiveLimits));
        }
        if self.max_allocation_bytes > MAX_ALLOCATION {
            return Err(limits_error(crate::InputDefect::AllocatorAllocationLimit));
        }
        if self.max_concurrent_calls > self.max_allocations {
            return Err(limits_error(crate::InputDefect::AllocatorConcurrentLimit));
        }
        if self
            .max_concurrent_calls
            .checked_mul(self.max_allocation_bytes)
            .is_none_or(|granted| granted > self.max_bytes)
        {
            return Err(limits_error(crate::InputDefect::AllocatorByteLimit));
        }
        Ok(self)
    }
}

/// What the SDK needs memory for: one frame it is about to receive.
///
/// Planar audio is laid out as a picture is, one channel to a line: its
/// [`width`](Self::width) is the number of samples per channel, its
/// [`height`](Self::height) the number of channels, and its
/// [`stride_bytes`](Self::stride_bytes) the distance between channels in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AllocationRequest {
    bytes: usize,
    stride: usize,
    format: AllocationFormat,
    width: u32,
    height: u32,
    capacity_limit: usize,
    audio_layout: Option<ValidatedAudioLayout>,
}

/// The kind of frame an [`AllocationRequest`] is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AllocationFormat {
    /// Uncompressed video in this pixel format.
    Video(PixelFormat),
    /// Uncompressed planar 32-bit floating-point audio.
    AudioPlanarF32,
    /// Compressed video with this FourCC.
    CompressedVideo(FourCc),
    /// Compressed audio with this FourCC.
    CompressedAudio(FourCc),
}

impl AllocationRequest {
    /// The largest buffer, in bytes, a policy may return for this request:
    /// [`AllocatorLimits::max_allocation_bytes`], or what is left of
    /// [`AllocatorLimits::max_bytes`] when that is less.
    pub fn capacity_limit(self) -> usize {
        self.capacity_limit
    }

    /// The smallest buffer, in bytes, that holds the frame at this request's
    /// stride.
    pub fn minimum_bytes(self) -> usize {
        self.bytes
    }

    /// Bytes per video line or per audio channel. For a compressed frame,
    /// which has no stride, this is its size.
    pub fn stride_bytes(self) -> usize {
        self.stride
    }

    /// The kind of frame.
    pub fn format(self) -> AllocationFormat {
        self.format
    }

    /// Width in pixels of video; samples per channel of audio.
    pub fn width(self) -> u32 {
        self.width
    }

    /// Height in pixels of video; channels of audio.
    pub fn height(self) -> u32 {
        self.height
    }

    /// The same request with a larger line or channel stride, for buffers
    /// that need padding or alignment. The minimum size grows to match.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`](crate::Error::InvalidInput) when the stride is
    /// smaller than requested, is not one the frame's layout allows (an audio
    /// stride is a multiple of four bytes; a 4:2:0 or UYVA video stride is even),
    /// a compressed frame is given a stride other than its size, or the buffer would exceed
    /// the [`capacity_limit`](Self::capacity_limit).
    pub fn with_stride_bytes(self, stride: usize) -> Result<Self> {
        let audio_layout = self
            .audio_layout
            .map(|layout| layout.with_channel_stride_bytes(stride))
            .transpose()
            .map_err(|cause| invalid_input(crate::InputDefect::Layout(cause)))?;
        if stride < self.stride {
            return Err(invalid_input(crate::InputDefect::AllocatorShortStride {
                stride,
                minimum: self.stride,
            }));
        }
        let bytes = match self.format {
            AllocationFormat::Video(pixel) => {
                ValidatedVideoLayout::new_uncompressed_checked(
                    pixel,
                    self.width,
                    self.height,
                    Some(stride),
                )
                // This is an input stride refusal, retaining the layout evidence.
                .map_err(|cause| invalid_input(crate::InputDefect::Layout(cause)))?
                .data_len_bytes()
            }
            AllocationFormat::AudioPlanarF32 => {
                let layout = audio_layout.expect("audio requests retain their validated layout");
                // The layout caps its active extent at 64 MiB and its stride at
                // i32::MAX. Including the final channel's padding still fits usize.
                layout.channel_stride_bytes() * layout.no_channels()
            }
            _ if stride == self.stride => self.bytes,
            _ => {
                return Err(invalid_input(
                    crate::InputDefect::CompressedAllocationStride,
                ))
            }
        };
        if bytes > self.capacity_limit {
            return Err(invalid_input(crate::InputDefect::AllocationTooLarge {
                bytes,
                limit: self.capacity_limit,
            }));
        }
        Ok(Self {
            bytes,
            stride,
            audio_layout,
            ..self
        })
    }
}

/// Memory from the application's own pool, lent to the SDK for one frame.
///
/// The value is dropped, on whichever thread the SDK frees the frame from,
/// when the SDK no longer needs the memory.
///
/// # Safety
///
/// From creation until the value is dropped:
///
/// * [`pointer`](Self::pointer) and [`capacity`](Self::capacity) never change;
/// * the memory is initialized, writable from any thread, addressable by the
///   CPU, aligned to at least 64 bytes and valid for `capacity` bytes;
/// * nothing else reads, writes, frees or remaps it.
///
/// Dropping the value must not panic.
pub unsafe trait ExternalAllocation: Send + 'static {
    /// The start of the memory.
    fn pointer(&self) -> NonNull<u8>;

    /// The size of the memory in bytes.
    fn capacity(&self) -> usize;
}

/// An address, capacity and pool-defined slot tag, without ownership of memory.
///
/// Copying this descriptor grants no permission to access or lend its memory.
/// Each successful [`AllocationBuffer::from_pool`] call establishes one exclusive
/// loan, and requires the caller to uphold that constructor's safety contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolBlock {
    pointer: NonNull<u8>,
    capacity: usize,
    tag: usize,
}
impl PoolBlock {
    /// Describe a pool slot without accessing or lending its memory.
    pub const fn new(pointer: NonNull<u8>, capacity: usize, tag: usize) -> Self {
        Self {
            pointer,
            capacity,
            tag,
        }
    }
    /// The slot's address; this descriptor grants no access to it.
    pub const fn pointer(self) -> NonNull<u8> {
        self.pointer
    }
    /// The described capacity in bytes.
    pub const fn capacity(self) -> usize {
        self.capacity
    }
    /// The pool-defined slot identifier returned unchanged at release.
    pub const fn tag(self) -> usize {
        self.tag
    }
}
// SAFETY: This is a copyable address descriptor, not a memory view or owning
// handle. Moving/sharing it never dereferences the address; lending it to the
// SDK requires from_pool's explicit unsafe per-loan validity/exclusivity proof.
unsafe impl Send for PoolBlock {}
// SAFETY: Sharing a descriptor grants no read/write/release operation on memory.
unsafe impl Sync for PoolBlock {}

/// A pool that reclaims exclusive blocks lent through [`AllocationBuffer::from_pool`].
///
/// # Safety
///
/// For each valid loan, the pool must keep its initialized, CPU-addressable,
/// 64-byte-aligned memory stable and valid for the block's capacity until release,
/// with no other access, remapping or reclamation. Release must not panic. It and
/// the pool's last destructor may run on any SDK free thread, or on the SDK's
/// allocating thread when publication refuses a block, without registry locks.
/// See [`ExternalAllocation`] for the equivalent arbitrary-handle contract.
pub unsafe trait ExternalPool: Send + Sync + 'static {
    /// Return a block after the SDK's exclusive access has ended, or after
    /// publication refused it before any SDK access.
    ///
    /// # Safety
    ///
    /// The block must name one unreleased loan from this pool. Its SDK access
    /// must have ended, and this call must be the only release of that loan.
    unsafe fn release(&self, block: PoolBlock);
}

struct PoolStorage {
    pool: Arc<dyn ExternalPool>,
    block: PoolBlock,
}
impl Drop for PoolStorage {
    fn drop(&mut self) {
        // SAFETY: from_pool's caller establishes one valid matching-pool loan.
        // This storage is moved, never cloned, and dropped exactly once after
        // SDK access ends or publication is refused. Registry reclamation runs
        // outside its locks and contains a misbehaving pool's panic.
        unsafe { self.pool.release(self.block) };
    }
}

/// Zeroed memory from the global allocator.
struct OwnedStorage {
    pointer: NonNull<u8>,
    layout: Layout,
}

impl OwnedStorage {
    /// Allocate `bytes` zeroed bytes aligned to 64.
    fn zeroed(bytes: usize) -> Result<Self> {
        let layout = Layout::from_size_align(bytes, 64)
            .ok()
            .filter(|layout| layout.size() != 0)
            .ok_or_else(|| invalid_input(crate::InputDefect::AllocationLayout { bytes }))?;
        // SAFETY: the layout was just checked not to be zero-sized.
        let pointer = NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or_else(|| {
            crate::error::allocation_failed(crate::AllocationKind::ReceiveBuffer, Some(bytes))
        })?;
        Ok(Self { pointer, layout })
    }
}

// SAFETY: the value owns its allocation and nothing else refers to it, so it
// can be released, as it can be used, from any thread.
unsafe impl Send for OwnedStorage {}

// SAFETY: the memory was allocated zeroed with 64-byte alignment for exactly
// `layout.size()` bytes, is owned by this value alone, and is released only
// when it is dropped.
unsafe impl ExternalAllocation for OwnedStorage {
    fn pointer(&self) -> NonNull<u8> {
        self.pointer
    }

    fn capacity(&self) -> usize {
        self.layout.size()
    }
}

impl Drop for OwnedStorage {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `alloc_zeroed` with this layout and is
        // released exactly once.
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
    }
}

/// The memory a [`ReceiveAllocator`] returns for one [`AllocationRequest`].
///
/// # Threading
///
/// A buffer is [`Send`]: a policy returns it on an SDK thread, and the SDK
/// may free it on another. It is not [`Sync`], because it can hold an
/// [`ExternalAllocation`], which needs only to be `Send`, and
/// [`capacity`](Self::capacity) asks that allocation through a shared
/// reference.
pub struct AllocationBuffer {
    // `ExternalAllocation` asks only for Send, so that a pool whose handle is
    // not Sync can lend memory, and `capacity` calls into it through `&self`;
    // Sync would let two threads make that call at once. Nothing needs it:
    // the registry keeps each buffer behind its mutex, which needs only Send,
    // and frees it by value.
    storage: AllocationStorage,
    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    request: AllocationRequest,
}

enum AllocationStorage {
    Owned(OwnedStorage),
    External(Box<dyn ExternalAllocation>),
    Pool(PoolStorage),
}

impl AllocationStorage {
    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    fn pointer(&self) -> NonNull<u8> {
        match self {
            Self::Owned(storage) => storage.pointer(),
            Self::External(storage) => storage.pointer(),
            Self::Pool(storage) => storage.block.pointer,
        }
    }

    fn capacity(&self) -> usize {
        match self {
            Self::Owned(storage) => storage.capacity(),
            Self::External(storage) => storage.capacity(),
            Self::Pool(storage) => storage.block.capacity,
        }
    }
}

impl std::fmt::Debug for AllocationBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let storage = match &self.storage {
            AllocationStorage::Owned(_) => "owned",
            AllocationStorage::External(_) => "external",
            AllocationStorage::Pool(_) => "pool",
        };
        f.debug_struct("AllocationBuffer")
            .field("storage", &storage)
            .finish_non_exhaustive()
    }
}

impl AllocationBuffer {
    /// Allocate zeroed, 64-byte-aligned memory of the request's minimum size.
    ///
    /// # Errors
    ///
    /// [`Error::AllocationFailed`](crate::Error::AllocationFailed) when the global
    /// allocator cannot supply that much memory.
    pub fn zeroed(request: AllocationRequest) -> Result<Self> {
        Ok(Self {
            storage: AllocationStorage::Owned(OwnedStorage::zeroed(request.bytes)?),
            #[cfg(any(test, ndi_has_advanced_receive_allocators))]
            request,
        })
    }

    /// Lend memory through an arbitrary application-owned handle.
    ///
    /// Boxes the handle once per buffer. For a pool whose block can be
    /// described as plain data, [`from_pool`](Self::from_pool) avoids that allocation.
    ///
    /// # Errors
    ///
    /// Returns the memory with [`Error::InvalidInput`](crate::Error::InvalidInput)
    /// when it is smaller than the request's minimum, larger than its capacity
    /// limit, or not aligned to 64 bytes, so that a pool gets its buffer back.
    pub fn from_external<S: ExternalAllocation>(
        request: AllocationRequest,
        storage: S,
    ) -> std::result::Result<Self, Rejected<S>> {
        let problem = external_problem(request, storage.capacity(), || storage.pointer());
        match problem {
            Some(problem) => Err(Rejected::new(invalid_input(problem), storage)),
            None => Ok(Self {
                storage: AllocationStorage::External(Box::new(storage)),
                #[cfg(any(test, ndi_has_advanced_receive_allocators))]
                request,
            }),
        }
    }

    /// Lend a plain pool block without allocating or calling pool accessors.
    ///
    /// Clones the pool's `Arc`; the buffer releases the block exactly once on
    /// drop, including when registry publication refuses it. Constructor failure
    /// returns the block in [`Rejected`] without releasing it.
    ///
    /// # Safety
    ///
    /// If validation succeeds, `block` must name an initialized, writable,
    /// CPU-addressable allocation from `pool`, aligned to 64 bytes and valid
    /// for its stated capacity. Until `pool.release(block)`, nothing else may
    /// read, write, free or remap it, or lend another copy of this descriptor.
    /// The block must remain valid even if the caller drops its own pool Arc.
    /// A `PoolBlock` is Copy and can be forged; validation checks geometry only,
    /// so this per-loan ownership obligation cannot be proved by a safe constructor.
    ///
    /// # Errors
    ///
    /// Returns the unchanged block with [`Error::InvalidInput`](crate::Error::InvalidInput)
    /// for insufficient/excessive capacity or misalignment, as `from_external` does.
    pub unsafe fn from_pool<P: ExternalPool>(
        request: AllocationRequest,
        pool: &Arc<P>,
        block: PoolBlock,
    ) -> std::result::Result<Self, Rejected<PoolBlock>> {
        if let Some(problem) = external_problem(request, block.capacity, || block.pointer) {
            return Err(Rejected::new(invalid_input(problem), block));
        }
        let pool: Arc<dyn ExternalPool> = pool.clone();
        Ok(Self {
            storage: AllocationStorage::Pool(PoolStorage { pool, block }),
            #[cfg(any(test, ndi_has_advanced_receive_allocators))]
            request,
        })
    }

    /// The size of the memory in bytes.
    pub fn capacity(&self) -> usize {
        self.storage.capacity()
    }
}

/// Shared validation preserves capacity-before-pointer inspection for arbitrary
/// external handles. Pool storage supplies those same quantities as plain data.
fn external_problem(
    request: AllocationRequest,
    capacity: usize,
    pointer: impl FnOnce() -> NonNull<u8>,
) -> Option<crate::InputDefect> {
    if capacity < request.bytes {
        Some(crate::InputDefect::ExternalAllocationTooSmall)
    } else if capacity > request.capacity_limit {
        Some(crate::InputDefect::ExternalAllocationTooLarge)
    } else if !(pointer().as_ptr() as usize).is_multiple_of(64) {
        Some(crate::InputDefect::ExternalAllocationAlignment)
    } else {
        None
    }
}

/// A policy that supplies the memory received frames are written into.
///
/// Any closure `Fn(AllocationRequest) -> Result<AllocationBuffer>` that is
/// `Send + Sync` is a policy.
///
/// The SDK calls the policy on its own threads, possibly several at once, and
/// stalls video or audio while a call blocks. An error or a panic makes that
/// one allocation fail; the SDK then drops the frame. Inspect
/// [`AllocatorService::usage`] for process-wide [`AllocationDecline`] counts,
/// including distinct policy-error and contained-panic counts.
/// Invalid media layouts are declined before calling the policy, including
/// planar audio requests above capture's 64 MiB frame limit.
///
/// A replaced policy is destroyed only after its replacement is installed.
/// With no outstanding allocations or policy calls, the replacing call drops
/// it. Otherwise the last call or allocation to release it does, possibly on
/// an SDK thread inside a free callback. Its destructor must not wait for
/// frames from that receiver.
pub trait ReceiveAllocator: Send + Sync + 'static {
    /// Supply memory for one frame.
    ///
    /// # Errors
    ///
    /// Any error declines the allocation.
    fn allocate(&self, request: AllocationRequest) -> Result<AllocationBuffer>;
}

impl<F: Fn(AllocationRequest) -> Result<AllocationBuffer> + Send + Sync + 'static> ReceiveAllocator
    for F
{
    fn allocate(&self, request: AllocationRequest) -> Result<AllocationBuffer> {
        self(request)
    }
}

/// One registered policy.
struct Generation {
    policy: ManuallyDrop<Box<dyn ReceiveAllocator>>,
    count: Arc<AtomicUsize>,
}

impl Drop for Generation {
    fn drop(&mut self) {
        // The generation stays counted while the policy's own destructor
        // runs, and for good if that panics: its cleanup may be incomplete.
        // SAFETY: the policy is taken exactly once, here.
        let policy = unsafe { ManuallyDrop::take(&mut self.policy) };
        if catch_unwind(AssertUnwindSafe(|| drop(policy))).is_ok() {
            self.count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// One allocation the SDK holds, and the generation that made it.
struct Lease {
    _buffer: AllocationBuffer,
    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    bytes: usize,
    _generation: Arc<Generation>,
}

#[cfg(any(test, ndi_has_advanced_receive_allocators))]
type Leases = super::reserved_map::ReservedMap<(usize, usize), Lease>;
// Without allocator callbacks no lease can be published, so retain the empty
// table without reserving a workspace that this build can never use.
#[cfg(not(any(test, ndi_has_advanced_receive_allocators)))]
type Leases = HashMap<(usize, usize), Lease>;

#[derive(Default)]
struct State {
    active: HashMap<usize, Arc<Generation>>,
    /// Outstanding allocations by (generation token, address).
    ///
    /// The SDK frees every allocation through the allocator pair that made
    /// it, passing that pair's opaque value, even after the pair has been
    /// replaced; the token therefore identifies the lease's generation at the
    /// free as it did at the allocation. Keeping it in the key means a free
    /// can release only an allocation of the generation it names, so a
    /// repeated free cannot release a later allocation that happens to reuse
    /// the address under another generation.
    leases: Leases,
    bytes: usize,
    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    reserved_bytes: usize,
    calls: usize,
    reclaiming: usize,
    declines: [u64; AllocationDecline::COUNT],
}

#[cfg(any(test, ndi_has_advanced_receive_allocators))]
impl State {
    fn decline(&mut self, reason: AllocationDecline) {
        let count = &mut self.declines[reason.index()];
        *count = count.saturating_add(1);
    }
}

/// Every generation and lease in the process.
struct Registry {
    limits: AllocatorLimits,
    state: Mutex<State>,
    generations: Arc<AtomicUsize>,
    next: AtomicUsize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Replace the value of `counter` with `next(value)` and return the value
/// that was replaced, or leave it alone and return `None` when `next`
/// declines the value it is shown.
fn advance(counter: &AtomicUsize, next: impl Fn(usize) -> Option<usize>) -> Option<usize> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let replacement = next(current)?;
        match counter.compare_exchange_weak(
            current,
            replacement,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(replaced) => return Some(replaced),
            Err(actual) => current = actual,
        }
    }
}

/// The policy call that an admission covers: its count and the bytes it was
/// granted stay charged until the call ends or its buffer is published.
#[cfg(any(test, ndi_has_advanced_receive_allocators))]
struct Reservation<'a> {
    registry: &'a Registry,
    bytes: usize,
}

#[cfg(any(test, ndi_has_advanced_receive_allocators))]
impl Reservation<'_> {
    /// End the call under a lock the caller already holds, so that the
    /// buffer it publishes is never charged beside its own grant.
    fn release(self, state: &mut State) {
        state.calls -= 1;
        state.reserved_bytes -= self.bytes;
        std::mem::forget(self);
    }
}

#[cfg(any(test, ndi_has_advanced_receive_allocators))]
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.registry.state);
        state.calls -= 1;
        state.reserved_bytes -= self.bytes;
    }
}

impl Registry {
    fn new(limits: AllocatorLimits) -> Result<Self> {
        let unavailable =
            |_| crate::error::allocation_failed(crate::AllocationKind::AllocatorLeases, None);
        #[cfg(any(test, ndi_has_advanced_receive_allocators))]
        let leases = Leases::try_with_capacity(limits.max_allocations).map_err(unavailable)?;
        #[cfg(not(any(test, ndi_has_advanced_receive_allocators)))]
        let leases = {
            let mut leases = Leases::new();
            leases
                .try_reserve(limits.max_allocations)
                .map_err(unavailable)?;
            leases
        };
        Ok(Self {
            limits,
            state: Mutex::new(State {
                leases,
                ..State::default()
            }),
            generations: Arc::new(AtomicUsize::new(0)),
            next: AtomicUsize::new(1),
        })
    }

    fn usage(&self) -> AllocatorUsage {
        let state = lock(&self.state);
        AllocatorUsage {
            generations: self.generations.load(Ordering::Acquire),
            active_generations: state.active.len(),
            retained_allocations: state.leases.len() + state.reclaiming,
            retained_bytes: state.bytes,
            calls: state.calls,
            reclaiming: state.reclaiming,
            declines: state.declines,
        }
    }

    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    fn decline(&self, reason: AllocationDecline) {
        lock(&self.state).decline(reason);
    }

    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    fn allocate_request(
        &self,
        token: usize,
        request: std::result::Result<AllocationRequest, AllocationDecline>,
    ) -> Option<(NonNull<u8>, i32)> {
        match request {
            Ok(request) => self.allocate(token, request),
            Err(reason) => {
                self.decline(reason);
                None
            }
        }
    }

    /// Register `policy` as a new generation and return its token, or return
    /// the policy when the process has no room for another.
    fn register<P: ReceiveAllocator>(&self, policy: P) -> std::result::Result<usize, Rejected<P>> {
        let token = match self.admit() {
            Ok(token) => token,
            Err(error) => return Err(Rejected::new(error, policy)),
        };
        let generation = Arc::new(Generation {
            policy: ManuallyDrop::new(Box::new(policy)),
            count: self.generations.clone(),
        });
        lock(&self.state).active.insert(token, generation);
        Ok(token)
    }

    /// Count one more generation and name it with a token that has never
    /// been used.
    fn admit(&self) -> Result<usize> {
        advance(&self.generations, |count| {
            (count < self.limits.max_generations).then_some(count + 1)
        })
        .ok_or_else(|| invalid_input(crate::InputDefect::AllocatorGenerationBudget))?;
        advance(&self.next, |token| token.checked_add(1)).ok_or_else(|| {
            self.generations.fetch_sub(1, Ordering::AcqRel);
            invalid_input(crate::InputDefect::AllocatorTokensExhausted)
        })
    }

    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    fn closing(&self, token: usize) {
        // Removing membership is the one transition: an admitted call retains
        // the generation, but can no longer publish. User policy destruction
        // runs after the state lock is released.
        let generation = lock(&self.state).active.remove(&token);
        drop(generation);
    }

    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    fn allocate(&self, token: usize, mut request: AllocationRequest) -> Option<(NonNull<u8>, i32)> {
        let (generation, reservation) = {
            let mut s = lock(&self.state);
            let Some(generation) = s.active.get(&token).cloned() else {
                s.decline(AllocationDecline::GenerationUnavailable);
                return None;
            };
            if s.calls >= self.limits.max_concurrent_calls {
                s.decline(AllocationDecline::ConcurrencyLimit);
                return None;
            }
            if s.leases.len() + s.calls + s.reclaiming >= self.limits.max_allocations {
                s.decline(AllocationDecline::AllocationCountLimit);
                return None;
            }
            // Grant the call its buffer before user code runs, so that the
            // policies running at once cannot return more than the budget
            // between them, padding included: the largest single allocation,
            // or whatever is left when that is less.
            let granted = self
                .limits
                .max_bytes
                .saturating_sub(s.bytes)
                .saturating_sub(s.reserved_bytes)
                .min(self.limits.max_allocation_bytes);
            if granted < request.bytes {
                s.decline(AllocationDecline::ByteLimit);
                return None;
            }
            s.calls += 1;
            s.reserved_bytes += granted;
            let reservation = Reservation {
                registry: self,
                bytes: granted,
            };
            (generation, reservation)
        };
        request.capacity_limit = reservation.bytes;
        // Storage accessors are application code too. A panic while checking a
        // returned buffer must be counted just like a panic in the policy,
        // before the outer native callback turns it into a refused request.
        let (allocation, pointer, capacity) = match catch_unwind(AssertUnwindSafe(|| {
            generation.policy.allocate(request).map(|allocation| {
                let pointer = allocation.storage.pointer();
                let capacity = allocation.capacity();
                (allocation, pointer, capacity)
            })
        })) {
            Ok(Ok(allocation)) => allocation,
            Ok(Err(_)) => {
                self.decline(AllocationDecline::PolicyError);
                return None;
            }
            Err(_) => {
                self.decline(AllocationDecline::PolicyPanic);
                return None;
            }
        };
        let actual = allocation.request;
        // The request's stride rule also defines the publication layout. A
        // compressed size must match exactly; uncompressed padding must produce
        // this same minimum extent. Capacity is a separate admission bound.
        let consistent = request
            .with_stride_bytes(actual.stride)
            .is_ok_and(|expected| {
                expected
                    == AllocationRequest {
                        capacity_limit: expected.capacity_limit,
                        ..actual
                    }
            });
        if capacity > reservation.bytes || !consistent || capacity < actual.bytes {
            self.decline(AllocationDecline::BufferRefused);
            return None;
        }
        // At most the capacity, which is at most MAX_ALLOCATION.
        let stride = if let Some(layout) = actual.audio_layout {
            layout.sdk_channel_stride()
        } else {
            let Ok(stride) = i32::try_from(actual.stride) else {
                self.decline(AllocationDecline::BufferRefused);
                return None;
            };
            stride
        };
        // Membership serializes publication against closing. An admitted policy
        // may have blocked while replacement began; it must not publish late.
        let mut s = lock(&self.state);
        if !s.active.contains_key(&token) {
            s.decline(AllocationDecline::GenerationUnavailable);
            return None;
        }
        let lease = Lease {
            _buffer: allocation,
            bytes: capacity,
            _generation: generation,
        };
        if let Err((_, lease)) = s
            .leases
            .insert_new((token, pointer.as_ptr() as usize), lease)
        {
            s.decline(AllocationDecline::BufferRefused);
            // Policy destructors must run after releasing the registry lock.
            drop(s);
            drop(lease);
            return None;
        }
        // The buffer takes the place of the grant that covered it.
        reservation.release(&mut s);
        s.bytes += capacity;
        drop(s);
        Some((pointer, stride))
    }

    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    fn free(&self, token: usize, pointer: *mut u8) -> bool {
        if pointer.is_null() {
            return true;
        }
        let lease = {
            let mut s = lock(&self.state);
            let lease = s.leases.remove(&(token, pointer as usize));
            if lease.is_some() {
                s.reclaiming += 1;
            }
            lease
        };
        match lease {
            Some(lease) => {
                let bytes = lease.bytes;
                // User storage reclamation runs without registry locks. Its
                // memory remains charged until reclamation has completed.
                if catch_unwind(AssertUnwindSafe(|| drop(lease))).is_err() {
                    // A panicking external destructor may have leaked storage.
                    // Keep its count/bytes charged permanently rather than allow
                    // repeated failures to evade the process memory bound.
                    return false;
                }
                let mut s = lock(&self.state);
                s.bytes -= bytes;
                s.reclaiming -= 1;
                true
            }
            None => false,
        }
    }
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// Permission to give receivers custom allocators.
///
/// The SDK may call an allocator's free function long after the receiver's
/// policy was replaced, so custom allocation needs the NDI runtime to stay
/// initialized until the process exits: the service is created from a
/// [`ProcessLifetimeRuntime`]. The first service fixes the
/// [`AllocatorLimits`] for the process, and later ones must name the same
/// limits.
///
/// A program that loads this crate as part of a plug-in must also keep that
/// module loaded for as long as the process runs, because the SDK holds
/// pointers to its callbacks.
#[derive(Clone, Copy)]
pub struct AllocatorService {
    runtime: ProcessLifetimeRuntime,
    registry: &'static Registry,
}

/// Why a custom receive allocation was declined and the SDK dropped its frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AllocationDecline {
    /// The SDK's format, dimensions or requested extent cannot be represented.
    /// Includes planar audio above capture's 64 MiB frame limit; the policy
    /// is not called and recording this decline allocates nothing.
    RequestNotRepresentable,
    /// The policy generation is unknown, replaced, closing or retired.
    GenerationUnavailable,
    /// The process already has its maximum number of concurrent policy calls.
    ConcurrencyLimit,
    /// Retained allocations, calls and reclamations fill the allocation count limit.
    AllocationCountLimit,
    /// The remaining total or per-allocation byte grant cannot hold the request.
    ByteLimit,
    /// The application policy returned an error.
    PolicyError,
    /// The application policy or its buffer inspection panicked; the callback
    /// contained the panic.
    PolicyPanic,
    /// The policy's buffer has an inconsistent size, layout, stride or live address.
    BufferRefused,
}

// Missing reasons make index() non-exhaustive; duplicates are unreachable
// patterns. The count and the test list therefore cannot drift from indexing.
macro_rules! allocation_declines {
    ($($reason:ident),+ $(,)?) => {
        impl AllocationDecline {
            const COUNT: usize = [$(Self::$reason),+].len();
            #[cfg(test)]
            const ALL: [Self; Self::COUNT] = [$(Self::$reason),+];
            const fn index(self) -> usize {
                enum Index { $($reason),+ }
                match self { $(Self::$reason => Index::$reason as usize),+ }
            }
        }
    };
}
allocation_declines!(
    RequestNotRepresentable,
    GenerationUnavailable,
    ConcurrencyLimit,
    AllocationCountLimit,
    ByteLimit,
    PolicyError,
    PolicyPanic,
    BufferRefused,
);

/// A snapshot of custom allocation across the process.
///
/// Decline counters include all generations since the service was created,
/// including retired policies. They saturate at `u64::MAX` and never reset.
/// Each declined SDK request increments exactly one reason; successful
/// allocations increment none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct AllocatorUsage {
    generations: usize,
    active_generations: usize,
    retained_allocations: usize,
    retained_bytes: usize,
    calls: usize,
    reclaiming: usize,
    declines: [u64; AllocationDecline::COUNT],
}

impl AllocatorUsage {
    /// All declined custom receive allocations across the process.
    ///
    /// The total saturates at `u64::MAX`.
    pub fn declined_allocations(self) -> u64 {
        self.declines.into_iter().fold(0, u64::saturating_add)
    }

    /// Declined custom receive allocations for one reason.
    pub fn declined(self, reason: AllocationDecline) -> u64 {
        self.declines[reason.index()]
    }

    /// Policies that exist, including replaced ones whose allocations the SDK
    /// still holds. A policy whose destructor panicked stays counted.
    pub fn generations(self) -> usize {
        self.generations
    }

    /// Policies currently registered with a receiver.
    pub fn active_generations(self) -> usize {
        self.active_generations
    }

    /// Allocations the SDK holds, including those being released.
    /// A release whose policy code panicked stays counted, because its storage
    /// may have leaked.
    pub fn retained_allocations(self) -> usize {
        self.retained_allocations
    }

    /// Bytes the SDK holds, including those being released.
    /// A release whose policy code panicked stays counted, because its storage
    /// may have leaked.
    pub fn retained_bytes(self) -> usize {
        self.retained_bytes
    }

    /// Policy calls running at this moment.
    pub fn in_progress_allocations(self) -> usize {
        self.calls
    }

    /// Buffers whose release is running at this moment.
    /// A release whose policy code panicked stays counted, because its storage
    /// may have leaked. This count therefore does not return to zero after
    /// such a panic.
    pub fn in_progress_reclamations(self) -> usize {
        self.reclaiming
    }
}

impl std::fmt::Debug for AllocatorService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllocatorService")
            .field("limits", &self.registry.limits)
            .finish_non_exhaustive()
    }
}

impl AllocatorService {
    /// Enable custom allocation with these process-wide limits.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`](crate::Error::InvalidInput) when the limits are
    /// out of range or differ from those of an earlier service.
    /// [`Error::AllocationFailed`](crate::Error::AllocationFailed) when the
    /// bounded lease bookkeeping cannot be reserved.
    pub fn new(runtime: ProcessLifetimeRuntime, limits: &AllocatorLimits) -> Result<Self> {
        let limits = (*limits).validate()?;
        let registry = match REGISTRY.get() {
            Some(registry) => registry,
            None => {
                // Reserve off the SDK's threads. If another caller initializes
                // first, its limits win and the comparison below is unchanged.
                let candidate = Registry::new(limits)?;
                REGISTRY.get_or_init(|| candidate)
            }
        };
        if registry.limits != limits {
            return Err(crate::Error::InvalidInput(
                crate::InputDefect::AllocatorLimitsDiffer,
            ));
        }
        Ok(Self { runtime, registry })
    }

    /// A consistent snapshot of retained storage, active calls and decline
    /// counters across the process. Counter updates allocate no storage and
    /// run under the same lock that accounts for retained allocations.
    pub fn usage(self) -> AllocatorUsage {
        self.registry.usage()
    }

    /// Take a receiver so that its allocators can be set.
    ///
    /// # Errors
    ///
    /// Returns the receiver, untouched, with
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime (or in a build without the Advanced SDK).
    pub fn attach(
        self,
        receiver: Receiver,
    ) -> std::result::Result<AllocatedReceiver, Rejected<Receiver>> {
        self.allocation(receiver)
    }

    /// Take a packet receiver so that its allocators can be set.
    ///
    /// # Errors
    ///
    /// As for [`attach`](Self::attach).
    pub fn attach_packets(
        self,
        receiver: PacketReceiver,
    ) -> std::result::Result<AllocatedReceiver<PacketAttached>, Rejected<PacketReceiver>> {
        self.allocation(receiver.receiver)
            .map_err(|rejected| rejected.map(PacketReceiver::from_receiver))
    }

    /// Take `receiver`, in any mode, with no allocator of its own set yet.
    fn allocation<M: ReceiverMode>(
        self,
        receiver: Receiver<M>,
    ) -> std::result::Result<AllocatedReceiver<M>, Rejected<Receiver<M>>> {
        match self.runtime.ndi().table::<ReceiveAllocators>() {
            Ok(table) => Ok(AllocatedReceiver {
                registration: Registration {
                    core: receiver.core.clone(),
                    table,
                    #[cfg(ndi_has_advanced_receive_allocators)]
                    video_token: None,
                    #[cfg(ndi_has_advanced_receive_allocators)]
                    audio_token: None,
                    service: self,
                },
                receiver,
            }),
            Err(error) => Err(Rejected::new(error, receiver)),
        }
    }
}

/// Which of a receiver's two allocators an operation concerns.
#[derive(Clone, Copy)]
enum Stream {
    Video,
    Audio,
}

/// A receiver whose frames can be received into application memory.
///
/// The default [`RawCapture`] mode retains ordinary receiver capture.
/// [`AllocatorService::attach_packets`] returns the [`PacketAttached`] mode,
/// which captures through [`capture_any`](AllocatedReceiver::capture_any) and
/// [`capture_any_ref`](AllocatedReceiver::capture_any_ref).
/// [`receiver`](Self::receiver) lends the receiver in its existing mode;
/// [`controls`](Self::controls) permits exclusive stream controls and KVM
/// without allowing that receiver to be replaced.
///
/// Dropping this owner, or taking the receiver back, returns it to the SDK's
/// own memory first. The receiver may live on in a
/// [`RecvAdvertiser`](crate::discovery_server::RecvAdvertiser) registration;
/// allocations it still holds are freed through the process-wide token registry.
/// The registration retains the exact receiver core, so resetting always
/// reaches the original native instance.
///
/// Setting an allocator replaces the previous one for frames allocated from
/// then on. Frames allocated earlier stay in the memory the earlier policy
/// supplied, and that policy is kept until the SDK has freed the last of
/// them. Only a call of the earlier policy still running when the replacement
/// is installed is declined.
///
/// # Threading
///
/// Both returned modes are [`Send`] and [`Sync`]: capturing, changing allocators
/// and dropping the owner can happen on any thread. Captures borrow it shared;
/// changing allocators requires an exclusive borrow. The SDK calls policies
/// on its own threads, possibly concurrently, and frees each allocation through
/// the callback that created it. Registry access is synchronized.
///
/// # Examples
///
/// ```no_run
/// use std::time::Duration;
/// use grafton_ndi::{
///     advanced::{AllocationBuffer, AllocationRequest, AllocatorLimits, AllocatorService},
///     Receiver, ReceiverOptions, Source, Ndi,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// # let source = Source::new("").unwrap();
/// let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
/// let service = AllocatorService::new(ndi.process_lifetime(), &AllocatorLimits::default())?;
/// let mut receiver = service.attach(receiver)?;
///
/// // Pad every video line to a multiple of 256 bytes.
/// receiver.set_video_allocator(|request: AllocationRequest| {
///     let stride = request.stride_bytes().next_multiple_of(256);
///     AllocationBuffer::zeroed(request.with_stride_bytes(stride)?)
/// })?;
/// if let Some(frame) = receiver.receiver().capture_video(Duration::from_secs(1))? {
///     println!("line stride: {} bytes", frame.line_stride_bytes());
/// }
/// # Ok(())
/// # }
/// ```
pub struct AllocatedReceiver<M: ReceiverMode = RawCapture> {
    registration: Registration,
    receiver: Receiver<M>,
}

impl<M: ReceiverMode> std::fmt::Debug for AllocatedReceiver<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllocatedReceiver")
            .field("mode", &std::any::type_name::<M>())
            .finish_non_exhaustive()
    }
}

impl<M: ReceiverMode> AllocatedReceiver<M> {
    /// The receiver, for capture permitted by its mode and shared controls.
    pub fn receiver(&self) -> &Receiver<M> {
        &self.receiver
    }

    /// Borrow exclusive stream controls or start KVM without permitting
    /// receiver replacement. This allocates nothing and makes no SDK call.
    /// Capture through the KVM session while inputs are held.
    pub fn controls(&mut self) -> ReceiverControls<'_, M> {
        ReceiverControls::new(&mut self.receiver)
    }

    /// Receive video into memory from `policy`.
    ///
    /// # Errors
    ///
    /// Returns the policy with [`Error::InvalidInput`](crate::Error::InvalidInput)
    /// when the process already has [`AllocatorLimits::max_generations`]
    /// policies. The current allocator then stays in place.
    #[doc(alias = "NDIlib_recv_set_video_allocator")]
    pub fn set_video_allocator<P: ReceiveAllocator>(
        &mut self,
        policy: P,
    ) -> std::result::Result<(), Rejected<P>> {
        self.set_allocator(Stream::Video, policy)
    }

    /// Receive audio into memory from `policy`.
    ///
    /// # Errors
    ///
    /// As for [`set_video_allocator`](Self::set_video_allocator).
    #[doc(alias = "NDIlib_recv_set_audio_allocator")]
    pub fn set_audio_allocator<P: ReceiveAllocator>(
        &mut self,
        policy: P,
    ) -> std::result::Result<(), Rejected<P>> {
        self.set_allocator(Stream::Audio, policy)
    }

    /// Return to the SDK's own video memory.
    #[doc(alias = "NDIlib_recv_set_video_allocator")]
    pub fn reset_video_allocator(&mut self) {
        self.registration.install(Stream::Video, None);
    }

    /// Return to the SDK's own audio memory.
    #[doc(alias = "NDIlib_recv_set_audio_allocator")]
    pub fn reset_audio_allocator(&mut self) {
        self.registration.install(Stream::Audio, None);
    }

    fn set_allocator<P: ReceiveAllocator>(
        &mut self,
        stream: Stream,
        policy: P,
    ) -> std::result::Result<(), Rejected<P>> {
        let token = self.registration.service.registry.register(policy)?;
        self.registration.install(stream, Some(token));
        Ok(())
    }
    fn release_receiver(self) -> Receiver<M> {
        let Self {
            registration,
            receiver,
        } = self;
        drop(registration);
        receiver
    }
}

impl AllocatedReceiver<RawCapture> {
    /// Return to the SDK's own memory and give the receiver back.
    pub fn into_receiver(self) -> Receiver {
        self.release_receiver()
    }
}

impl AllocatedReceiver<PacketAttached> {
    /// Wait up to `timeout` for the next frame or event; see
    /// [`PacketReceiver::capture_any`].
    ///
    /// # Errors
    ///
    /// As for [`PacketReceiver::capture_any`], including [`crate::Error::UnexpectedSdkResult`]
    /// once per failed status publication, whose event remains pending.
    #[doc(alias = "NDIlib_recv_capture_v3")]
    pub fn capture_any(&self, timeout: std::time::Duration) -> Result<Option<PacketItem>> {
        PacketReceiver::capture_from(self.receiver(), timeout)
    }

    /// Borrow a packet directly from SDK or application-provided storage.
    /// A held item keeps its custom allocation charged until the SDK releases
    /// it after the item is dropped. See [`PacketReceiver::capture_any_ref`].
    ///
    /// # Errors
    ///
    /// As for [`PacketReceiver::capture_any_ref`], including [`crate::Error::UnexpectedSdkResult`]
    /// once per failed status publication, whose event remains pending.
    #[doc(alias = "NDIlib_recv_capture_v3")]
    pub fn capture_any_ref(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Option<PacketItemRef<'_>>> {
        PacketReceiver::capture_ref_from(self.receiver(), timeout)
    }

    /// Return to the SDK's own memory and give the packet receiver back.
    pub fn into_packet_receiver(self) -> PacketReceiver {
        PacketReceiver::from_receiver(self.release_receiver())
    }
}

/// Owns the exact receiver and runtime needed to uninstall both callback pairs.
/// Its own Drop keeps reset before native destruction independent of field order.
struct Registration {
    #[cfg_attr(
        not(ndi_has_advanced_receive_allocators),
        expect(
            dead_code,
            reason = "the uninhabited table prevents registrations without declarations"
        )
    )]
    core: Arc<ReceiverCore>,
    table: &'static ReceiveAllocators,
    #[cfg(ndi_has_advanced_receive_allocators)]
    video_token: Option<usize>,
    #[cfg(ndi_has_advanced_receive_allocators)]
    audio_token: Option<usize>,
    service: AllocatorService,
}
impl Registration {
    /// Register generation `token` (or the SDK's own allocator) for `stream`
    /// and retire the generation it replaces.
    #[cfg(ndi_has_advanced_receive_allocators)]
    fn install(&mut self, stream: Stream, token: Option<usize>) {
        let instance = self.core.instance.0;
        let table = self.table;
        let slot = match stream {
            Stream::Video => &mut self.video_token,
            Stream::Audio => &mut self.audio_token,
        };
        let opaque = token.map_or(std::ptr::null_mut(), |token| token as *mut c_void);
        // SAFETY: the registration owns the original receiver core, so that native
        // instance stays live through installation and reset. The callbacks are functions of this
        // program, which stays loaded, and the opaque value is a token they
        // look up, never a pointer they follow.
        install_with(self.service.registry, slot, token, || unsafe {
            match stream {
                Stream::Video => table.NDIlib_recv_set_video_allocator(
                    instance,
                    opaque,
                    token.map(|_| video_allocate as _),
                    token.map(|_| video_free as _),
                ),
                Stream::Audio => table.NDIlib_recv_set_audio_allocator(
                    instance,
                    opaque,
                    token.map(|_| audio_allocate as _),
                    token.map(|_| audio_free as _),
                ),
            }
        });
    }

    // A build without this declaration family cannot create an Allocation.
    // Its uninhabited table proves that no allocator can be installed here.
    #[cfg(not(ndi_has_advanced_receive_allocators))]
    fn install(&mut self, _: Stream, _: Option<usize>) {
        match *self.table {}
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.install(Stream::Video, None);
        self.install(Stream::Audio, None);
    }
}

/// Install the native pair before retiring its predecessor. The setter may
/// still call the earlier policy; its allocations remain admitted until the
/// pair has been replaced. Retirement then prevents unfinished calls from
/// publishing and can destroy a policy only after the SDK has moved on.
#[cfg(any(test, ndi_has_advanced_receive_allocators))]
fn install_with(
    registry: &Registry,
    slot: &mut Option<usize>,
    token: Option<usize>,
    set: impl FnOnce(),
) {
    set();
    if let Some(earlier) = std::mem::replace(slot, token) {
        registry.closing(earlier);
    }
}

/// A compressed frame's size, which the SDK passes in the stride union.
#[cfg(any(test, ndi_has_advanced_receive_allocators))]
fn compressed_size(size: i32) -> std::result::Result<usize, AllocationDecline> {
    usize::try_from(size)
        .ok()
        .filter(|size| (1..=MAX_ALLOCATION).contains(size))
        .ok_or(AllocationDecline::RequestNotRepresentable)
}

/// A dimension of a compressed frame the SDK is about to receive.
#[cfg(any(test, ndi_has_advanced_receive_allocators))]
fn compressed_dimension(value: i32) -> std::result::Result<u32, AllocationDecline> {
    u32::try_from(value).map_err(|_| AllocationDecline::RequestNotRepresentable)
}

/// The request for the video frame the SDK is about to receive.
#[cfg(any(test, ndi_has_advanced_receive_allocators))]
fn video_request(
    frame: &NDIlib_video_frame_v2_t,
) -> std::result::Result<AllocationRequest, AllocationDecline> {
    let fourcc = FourCc::from_raw(frame.FourCC);
    if VideoCodec::from_outer(frame.FourCC).is_some() {
        // SAFETY: for a compressed frame the union holds the data size.
        let size = compressed_size(unsafe { frame.__bindgen_anon_1.data_size_in_bytes })?;
        return Ok(AllocationRequest {
            bytes: size,
            stride: size,
            format: AllocationFormat::CompressedVideo(fourcc),
            width: compressed_dimension(frame.xres)?,
            height: compressed_dimension(frame.yres)?,
            capacity_limit: MAX_ALLOCATION,
            audio_layout: None,
        });
    }
    let pixel =
        PixelFormat::from_fourcc(fourcc).ok_or(AllocationDecline::RequestNotRepresentable)?;
    let layout = ValidatedVideoLayout::from_sdk_checked(pixel, frame.xres, frame.yres, 0)
        .map_err(|_| AllocationDecline::RequestNotRepresentable)?;
    Ok(AllocationRequest {
        bytes: layout.data_len_bytes(),
        stride: layout.line_stride_bytes(),
        format: AllocationFormat::Video(pixel),
        width: layout.width(),
        height: layout.height(),
        capacity_limit: MAX_ALLOCATION,
        audio_layout: None,
    })
}

/// The request for the audio frame the SDK is about to receive.
#[cfg(any(test, ndi_has_advanced_receive_allocators))]
fn audio_request(
    frame: &NDIlib_audio_frame_v3_t,
) -> std::result::Result<AllocationRequest, AllocationDecline> {
    if frame.FourCC != crate::FourCc::from(crate::AudioFormat::Fltp).raw() {
        if AudioCodec::from_outer(frame.FourCC).is_none() {
            return Err(AllocationDecline::RequestNotRepresentable);
        }
        // SAFETY: for compressed audio the union holds the exact packet size;
        // tests/advanced_runtime.rs::compressed_audio_packets_use_exact_custom_allocator_grants
        // pins the AAC/Opus request format, extent and delivered application memory.
        let size = compressed_size(unsafe { frame.__bindgen_anon_1.data_size_in_bytes })?;
        return Ok(AllocationRequest {
            bytes: size,
            stride: size,
            format: AllocationFormat::CompressedAudio(FourCc::from_raw(frame.FourCC)),
            width: compressed_dimension(frame.no_samples)?,
            height: compressed_dimension(frame.no_channels)?,
            capacity_limit: MAX_ALLOCATION,
            audio_layout: None,
        });
    }
    // The callback chooses a packed stride. The incoming data pointer and
    // stride do not name allocated storage yet and must not be read as media.
    let layout = ValidatedAudioLayout::from_sdk_checked(
        AudioFormat::Fltp,
        frame.sample_rate,
        frame.no_channels,
        frame.no_samples,
        None,
    )
    .map_err(|_| AllocationDecline::RequestNotRepresentable)?;
    Ok(AllocationRequest {
        bytes: layout.sample_count() * size_of::<f32>(),
        stride: layout.channel_stride_bytes(),
        format: AllocationFormat::AudioPlanarF32,
        width: layout.num_samples(),
        height: layout.num_channels(),
        capacity_limit: MAX_ALLOCATION,
        audio_layout: Some(layout),
    })
}
/// The SDK's request for video memory. `opaque` is the generation token.
#[cfg(ndi_has_advanced_receive_allocators)]
unsafe extern "C" fn video_allocate(
    opaque: *mut c_void,
    frame: *mut NDIlib_video_frame_v2_t,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(registry) = REGISTRY.get() else {
            return false;
        };
        // SAFETY: the SDK passes null or a descriptor it owns for this call.
        let Some(frame) = (unsafe { frame.as_mut() }) else {
            registry.decline(AllocationDecline::RequestNotRepresentable);
            return false;
        };
        let request = video_request(frame);
        frame.p_data = std::ptr::null_mut();
        frame.__bindgen_anon_1.line_stride_in_bytes = 0;
        let Some((pointer, stride)) = registry.allocate_request(opaque as usize, request) else {
            return false;
        };
        frame.p_data = pointer.as_ptr();
        frame.__bindgen_anon_1.line_stride_in_bytes = stride;
        true
    }))
    .unwrap_or(false)
}

/// The SDK's request for audio memory. `opaque` is the generation token.
#[cfg(ndi_has_advanced_receive_allocators)]
unsafe extern "C" fn audio_allocate(
    opaque: *mut c_void,
    frame: *mut NDIlib_audio_frame_v3_t,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(registry) = REGISTRY.get() else {
            return false;
        };
        // SAFETY: the SDK passes null or a descriptor it owns for this call.
        let Some(frame) = (unsafe { frame.as_mut() }) else {
            registry.decline(AllocationDecline::RequestNotRepresentable);
            return false;
        };
        let request = audio_request(frame);
        frame.p_data = std::ptr::null_mut();
        frame.__bindgen_anon_1.channel_stride_in_bytes = 0;
        let Some((pointer, stride)) = registry.allocate_request(opaque as usize, request) else {
            return false;
        };
        frame.p_data = pointer.as_ptr();
        frame.__bindgen_anon_1.channel_stride_in_bytes = stride;
        true
    }))
    .unwrap_or(false)
}

/// Release the allocation at `data` that generation `opaque` made.
#[cfg(ndi_has_advanced_receive_allocators)]
fn free(opaque: *mut c_void, data: *mut u8) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        REGISTRY
            .get()
            .is_some_and(|registry| registry.free(opaque as usize, data))
    }))
    .unwrap_or(false)
}

/// The SDK's release of video memory. `opaque` is the generation token.
#[cfg(ndi_has_advanced_receive_allocators)]
unsafe extern "C" fn video_free(
    opaque: *mut c_void,
    frame: *const NDIlib_video_frame_v2_t,
) -> bool {
    // SAFETY: the SDK passes null or a descriptor that is valid for this call.
    unsafe { frame.as_ref() }.is_some_and(|frame| free(opaque, frame.p_data))
}

/// The SDK's release of audio memory. `opaque` is the generation token.
#[cfg(ndi_has_advanced_receive_allocators)]
unsafe extern "C" fn audio_free(
    opaque: *mut c_void,
    frame: *const NDIlib_audio_frame_v3_t,
) -> bool {
    // SAFETY: the SDK passes null or a descriptor that is valid for this call.
    unsafe { frame.as_ref() }.is_some_and(|frame| free(opaque, frame.p_data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::StreamTier;
    include!("allocators/cycle_cost_tests.rs");
    /// Reusable initialized aligned blocks; the bounded free list grants one
    /// exclusive slot per claim and returns it only after lease reclamation.
    struct TestPool {
        _storage: Mutex<Vec<OwnedStorage>>,
        blocks: Vec<PoolBlock>,
        free: Mutex<std::collections::VecDeque<usize>>,
        releases: AtomicUsize,
    }
    impl TestPool {
        fn new(count: usize, bytes: usize) -> Arc<Self> {
            let storage = (0..count)
                .map(|_| OwnedStorage::zeroed(bytes).unwrap())
                .collect::<Vec<_>>();
            let blocks = storage
                .iter()
                .enumerate()
                .map(|(tag, storage)| PoolBlock::new(storage.pointer(), bytes, tag))
                .collect();
            let pool = Arc::new(Self {
                _storage: Mutex::new(storage),
                blocks,
                free: Mutex::new((0..count).collect()),
                releases: AtomicUsize::new(0),
            });
            // Apple pthread mutexes allocate on first lock. Pool setup belongs
            // outside callback allocation counts and cycle measurements.
            drop(lock(&pool.free));
            pool
        }
        fn claim(&self) -> PoolBlock {
            self.blocks[lock(&self.free)
                .pop_front()
                .expect("benchmark/test has enough exclusive slots")]
        }
        fn buffer(self: &Arc<Self>, request: AllocationRequest) -> Result<AllocationBuffer> {
            let block = self.claim();
            // SAFETY: claim removes one initialized aligned block from the free
            // list; this Arc owns its storage until release returns that slot.
            match unsafe { AllocationBuffer::from_pool(request, self, block) } {
                Ok(buffer) => Ok(buffer),
                Err(rejected) => {
                    let (error, block) = rejected.into_parts();
                    // SAFETY: refusal returned the same unpublished exclusive loan.
                    unsafe { self.release(block) };
                    Err(error)
                }
            }
        }
    }
    // SAFETY: storage is owned by this pool, initialized and aligned. The free
    // list grants exclusive slots, and storage stays live through each Arc loan.
    unsafe impl ExternalPool for TestPool {
        unsafe fn release(&self, block: PoolBlock) {
            assert_eq!(self.blocks[block.tag], block);
            let mut free = lock(&self.free);
            assert!(!free.contains(&block.tag), "a block must be released once");
            free.push_back(block.tag);
            self.releases.fetch_add(1, Ordering::Relaxed);
        }
    }
    struct PoolHandle {
        pool: Arc<TestPool>,
        block: PoolBlock,
    }
    // SAFETY: a handle owns one exclusively claimed TestPool slot until Drop.
    unsafe impl ExternalAllocation for PoolHandle {
        fn pointer(&self) -> NonNull<u8> {
            self.block.pointer
        }
        fn capacity(&self) -> usize {
            self.block.capacity
        }
    }
    impl Drop for PoolHandle {
        fn drop(&mut self) {
            // SAFETY: this non-Clone handle owns the only release of its claim.
            unsafe { self.pool.release(self.block) };
        }
    }

    #[test]
    fn pool_constructor_returns_refused_blocks_without_releasing_them() {
        let pool = TestPool::new(1, 64);
        let original = pool.claim();
        let request = AllocationRequest {
            capacity_limit: 64,
            ..request()
        };
        for (block, expected) in [
            (
                PoolBlock::new(original.pointer, 63, original.tag),
                crate::InputDefect::ExternalAllocationTooSmall,
            ),
            (
                PoolBlock::new(original.pointer, 65, original.tag),
                crate::InputDefect::ExternalAllocationTooLarge,
            ),
            (
                PoolBlock::new(
                    NonNull::new((original.pointer.as_ptr() as usize + 1) as *mut u8).unwrap(),
                    64,
                    original.tag,
                ),
                crate::InputDefect::ExternalAllocationAlignment,
            ),
        ] {
            // SAFETY: these intentionally invalid descriptors all fail validation
            // before a loan is accepted; none is accessed, published or released.
            let rejected = unsafe { AllocationBuffer::from_pool(request, &pool, block) }
                .expect_err("invalid block must be refused");
            assert!(matches!(rejected.error(), Error::InvalidInput(actual) if actual == &expected));
            assert_eq!(rejected.into_inner(), block);
            assert_eq!(pool.releases.load(Ordering::Relaxed), 0);
        }
        // SAFETY: the original claim was never lent and is returned once.
        unsafe { pool.release(original) };
    }

    #[test]
    fn pool_policy_constructs_buffers_without_allocating_but_external_handles_box_once() {
        for external in [false, true] {
            let registry = Registry::new(limits()).unwrap();
            let pool = TestPool::new(1, 64);
            let policy_pool = pool.clone();
            let token = registry
                .register(move |request| {
                    if external {
                        let block = policy_pool.claim();
                        Ok(AllocationBuffer::from_external(
                            request,
                            PoolHandle {
                                pool: policy_pool.clone(),
                                block,
                            },
                        )?)
                    } else {
                        policy_pool.buffer(request)
                    }
                })
                .unwrap();
            let (published, allocations) =
                crate::test_alloc::allocation_count(|| registry.allocate(token, request()));
            assert_eq!(allocations, usize::from(external));
            let (pointer, _) = published.unwrap();
            assert!(registry.free(token, pointer.as_ptr()));
            assert_eq!(pool.releases.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn pool_blocks_are_released_once_when_publication_refuses_them() {
        for retired in [false, true] {
            let registry = Arc::new(Registry::new(limits()).unwrap());
            let pool = TestPool::new(1, 64);
            let policy_pool = pool.clone();
            let weak_registry = Arc::downgrade(&registry);
            let name = Arc::new(AtomicUsize::new(0));
            let policy_name = name.clone();
            let token = registry
                .register(move |mut request: AllocationRequest| {
                    if retired {
                        weak_registry
                            .upgrade()
                            .unwrap()
                            .closing(policy_name.load(Ordering::Relaxed));
                    } else {
                        request.width += 1;
                    } // A buffer for a different layout.
                    policy_pool.buffer(request)
                })
                .unwrap();
            name.store(token, Ordering::Relaxed);
            assert!(registry.allocate(token, request()).is_none());
            assert_eq!(pool.releases.load(Ordering::Relaxed), 1);
            assert_eq!(registry.usage().retained_allocations(), 0);
            assert_eq!(
                registry.usage().declined(if retired {
                    AllocationDecline::GenerationUnavailable
                } else {
                    AllocationDecline::BufferRefused
                }),
                1
            );
        }
    }

    #[test]
    fn panicking_pool_release_is_contained_and_keeps_bytes_charged() {
        struct Panics(Arc<TestPool>);
        // Intentionally violates the release no-panic rule to test containment.
        unsafe impl ExternalPool for Panics {
            unsafe fn release(&self, _: PoolBlock) {
                panic!("contained pool release");
            }
        }
        let pool = Arc::new(Panics(TestPool::new(1, 64)));
        let policy_pool = pool.clone();
        let registry = Registry::new(limits()).unwrap();
        let token = registry
            .register(move |request| {
                let block = policy_pool.0.claim();
                // SAFETY: the nested pool grants initialized stable exclusive memory;
                // this test intentionally misbehaves only at release, after SDK use.
                Ok(unsafe { AllocationBuffer::from_pool(request, &policy_pool, block) }?)
            })
            .unwrap();
        let (pointer, _) = registry.allocate(token, request()).unwrap();
        assert!(!registry.free(token, pointer.as_ptr()));
        assert_eq!(registry.usage().retained_bytes(), 64);
        assert_eq!(registry.usage().in_progress_reclamations(), 1);
        assert_eq!(registry.usage().retained_allocations(), 1);
    }

    #[test]
    fn every_decline_reason_has_one_counter() {
        let registry = Registry::new(limits()).unwrap();
        for reason in AllocationDecline::ALL {
            registry.decline(reason);
        }
        let usage = registry.usage();
        assert_eq!(
            usage.declined_allocations(),
            AllocationDecline::COUNT as u64
        );
        for reason in AllocationDecline::ALL {
            assert_eq!(usage.declined(reason), 1);
        }
    }

    #[test]
    fn panicking_policy_destruction_keeps_its_generation_budget_charged() {
        struct Panics;
        impl ReceiveAllocator for Panics {
            fn allocate(&self, request: AllocationRequest) -> Result<AllocationBuffer> {
                AllocationBuffer::zeroed(request)
            }
        }
        impl Drop for Panics {
            fn drop(&mut self) {
                panic!("contained policy destructor");
            }
        }
        let registry = Registry::new(limits()).unwrap();
        let earlier = registry.register(Panics).unwrap();
        let replacement = registry.register(AllocationBuffer::zeroed).unwrap();
        let mut slot = Some(earlier);
        install_with(&registry, &mut slot, Some(replacement), || {});
        assert_eq!(registry.usage().active_generations(), 1);
        assert_eq!(registry.usage().generations(), 2);
        registry.closing(replacement);
        assert_eq!(registry.usage().active_generations(), 0);
        assert_eq!(registry.usage().generations(), 1);
    }

    #[test]
    fn native_allocation_formats_preserve_protection_and_decoded_formats() {
        use crate::advanced::packets::PROTECTED;
        let protected_video = VideoCodec::H264.outer(StreamTier::Program, true);
        let mut video = NDIlib_video_frame_v2_t {
            FourCC: protected_video,
            xres: 16,
            yres: 16,
            ..Default::default()
        };
        video.__bindgen_anon_1.data_size_in_bytes = 64;
        let request = video_request(&video).unwrap();
        assert_eq!(
            request.format(),
            AllocationFormat::CompressedVideo(FourCc::from_raw(protected_video))
        );
        assert_eq!(
            (request.width(), request.height(), request.stride_bytes()),
            (16, 16, 64)
        );
        video.xres = -16;
        assert!(matches!(
            video_request(&video),
            Err(AllocationDecline::RequestNotRepresentable)
        ));
        video.xres = 16;
        video.__bindgen_anon_1.data_size_in_bytes = 0;
        assert!(video_request(&video).is_err());
        video.FourCC = crate::FourCc::from(PixelFormat::Bgra).raw();
        let request = video_request(&video).unwrap();
        assert_eq!(request.format(), AllocationFormat::Video(PixelFormat::Bgra));
        assert_eq!(
            (request.width(), request.height(), request.stride_bytes()),
            (16, 16, 64)
        );
        assert_eq!(request.minimum_bytes(), 16 * 64);
        video.FourCC = u32::MAX;
        assert!(video_request(&video).is_err());
        for codec in [AudioCodec::Aac, AudioCodec::Opus] {
            for protected in [false, true] {
                let bits = codec.outer(protected);
                let mut audio = NDIlib_audio_frame_v3_t {
                    FourCC: bits,
                    ..Default::default()
                };
                audio.__bindgen_anon_1.data_size_in_bytes = 64;
                assert_eq!(
                    audio_request(&audio).unwrap().format(),
                    AllocationFormat::CompressedAudio(FourCc::from_raw(bits))
                );
                audio.FourCC = bits ^ PROTECTED ^ 1;
                assert!(audio_request(&audio).is_err());
            }
        }
    }

    fn limits() -> AllocatorLimits {
        AllocatorLimits {
            max_generations: 2,
            max_allocations: 2,
            max_bytes: 128,
            max_concurrent_calls: 2,
            max_allocation_bytes: 64,
        }
        .validate()
        .unwrap()
    }

    fn request_of(bytes: usize) -> AllocationRequest {
        AllocationRequest {
            bytes,
            stride: bytes / 2,
            format: AllocationFormat::AudioPlanarF32,
            width: (bytes / 8) as u32,
            height: 2,
            capacity_limit: MAX_ALLOCATION,
            audio_layout: Some(
                ValidatedAudioLayout::new_uncompressed(
                    AudioFormat::Fltp,
                    48_000,
                    2,
                    (bytes / 8) as u32,
                    None,
                )
                .unwrap(),
            ),
        }
    }

    fn request() -> AllocationRequest {
        request_of(64)
    }

    #[test]
    fn request_conversion_declines_count_once_without_calling_a_policy() {
        let registry = Registry::new(limits()).unwrap();
        let token = registry.register(AllocationBuffer::zeroed).unwrap();
        let video = NDIlib_video_frame_v2_t {
            FourCC: u32::MAX,
            ..Default::default()
        };
        let audio = NDIlib_audio_frame_v3_t {
            FourCC: u32::MAX,
            ..Default::default()
        };
        assert!(registry
            .allocate_request(token, video_request(&video))
            .is_none());
        assert!(registry
            .allocate_request(token, audio_request(&audio))
            .is_none());
        let usage = registry.usage();
        assert_eq!(
            usage.declined(AllocationDecline::RequestNotRepresentable),
            2
        );
        assert_eq!(usage.declined_allocations(), 2);
        assert_eq!(usage.in_progress_allocations(), 0);
        assert_eq!(usage.retained_allocations(), 0);
        let (pointer, _) = registry.allocate(token, request()).unwrap();
        assert_eq!(registry.usage().declined_allocations(), 2);
        assert!(registry.free(token, pointer.as_ptr()));
    }

    #[test]
    fn unrepresentable_requests_decline_without_allocating_or_calling_the_policy() {
        let registry = Registry::new(limits()).unwrap();
        let policy_calls = Arc::new(AtomicUsize::new(0));
        let calls = policy_calls.clone();
        let token = registry
            .register(move |_: AllocationRequest| -> Result<AllocationBuffer> {
                calls.fetch_add(1, Ordering::Relaxed);
                Err(crate::error::allocation_failed(
                    crate::AllocationKind::AllocatorLeases,
                    None,
                ))
            })
            .unwrap();
        let video = NDIlib_video_frame_v2_t {
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            xres: 16,
            yres: 16,
            ..Default::default()
        };
        let videos = [
            NDIlib_video_frame_v2_t {
                FourCC: u32::MAX,
                ..video
            },
            NDIlib_video_frame_v2_t { xres: -1, ..video },
            NDIlib_video_frame_v2_t { xres: 3, ..video },
            NDIlib_video_frame_v2_t { yres: 0, ..video },
            NDIlib_video_frame_v2_t {
                FourCC: FourCc::from(PixelFormat::I420).raw(),
                yres: 3,
                ..video
            },
            NDIlib_video_frame_v2_t {
                FourCC: FourCc::from(PixelFormat::Nv12).raw(),
                yres: 3,
                ..video
            },
            NDIlib_video_frame_v2_t {
                xres: i32::MAX - 1,
                yres: i32::MAX,
                ..video
            },
            NDIlib_video_frame_v2_t {
                FourCC: VideoCodec::H264.outer(StreamTier::Program, false),
                ..video
            },
        ];
        // FLTP extents are multiples of four: the first representable excess
        // over the 64 MiB capture cap is four bytes, not one byte.
        let audio = NDIlib_audio_frame_v3_t {
            FourCC: crate::FourCc::from(crate::AudioFormat::Fltp).raw(),
            sample_rate: 48_000,
            no_channels: 1,
            no_samples: (crate::frames::MAX_AUDIO_BYTES / size_of::<f32>()) as i32 + 1,
            ..Default::default()
        };
        let audios = [
            audio,
            NDIlib_audio_frame_v3_t {
                FourCC: u32::MAX,
                ..audio
            },
            NDIlib_audio_frame_v3_t {
                sample_rate: 0,
                ..audio
            },
            NDIlib_audio_frame_v3_t {
                no_channels: 0,
                ..audio
            },
            NDIlib_audio_frame_v3_t {
                no_samples: 0,
                ..audio
            },
            NDIlib_audio_frame_v3_t {
                no_samples: i32::MAX,
                ..audio
            },
            NDIlib_audio_frame_v3_t {
                FourCC: AudioCodec::Aac.outer(false),
                no_channels: -1,
                __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                    data_size_in_bytes: 64,
                },
                ..audio
            },
        ];
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for frame in &videos {
                assert!(registry
                    .allocate_request(token, video_request(frame))
                    .is_none());
            }
            for frame in &audios {
                assert!(registry
                    .allocate_request(token, audio_request(frame))
                    .is_none());
            }
        });
        assert_eq!(allocations, 0);
        assert_eq!(policy_calls.load(Ordering::Relaxed), 0);
        let usage = registry.usage();
        assert_eq!(
            usage.declined(AllocationDecline::RequestNotRepresentable),
            (videos.len() + audios.len()) as u64
        );
        assert_eq!(usage.in_progress_allocations(), 0);
        assert_eq!(usage.retained_allocations(), 0);
    }

    #[test]
    fn audio_request_at_capture_limit_uses_the_shared_packed_layout() {
        let frame = NDIlib_audio_frame_v3_t {
            FourCC: crate::FourCc::from(crate::AudioFormat::Fltp).raw(),
            sample_rate: 48_000,
            no_channels: 1,
            no_samples: (crate::frames::MAX_AUDIO_BYTES / size_of::<f32>()) as i32,
            // Allocator input is geometry, not existing media. The callback
            // chooses the packed stride without requiring a data pointer.
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: -1,
            },
            ..Default::default()
        };
        let (request, allocations) = crate::test_alloc::allocation_count(|| audio_request(&frame));
        let request = request.unwrap();
        assert_eq!(allocations, 0);
        let layout = ValidatedAudioLayout::from_sdk_checked(
            AudioFormat::Fltp,
            frame.sample_rate,
            frame.no_channels,
            frame.no_samples,
            None,
        )
        .unwrap();
        assert_eq!(request.minimum_bytes(), crate::frames::MAX_AUDIO_BYTES);
        assert_eq!(request.stride_bytes(), layout.channel_stride_bytes());
        assert_eq!(request.width(), layout.num_samples());
        assert_eq!(request.height(), layout.num_channels());
    }

    #[test]
    fn successful_owned_allocation_allocates_only_its_frame_memory() {
        let registry = Registry::new(limits()).unwrap();
        let token = registry.register(AllocationBuffer::zeroed).unwrap();
        let (published, allocations) =
            crate::test_alloc::allocation_count(|| registry.allocate(token, request()));
        let (pointer, _) = published.unwrap();
        assert_eq!(allocations, 1);
        assert!(registry.free(token, pointer.as_ptr()));
    }

    #[test]
    fn successful_preboxed_external_allocation_needs_no_bookkeeping_allocation() {
        let registry = Registry::new(limits()).unwrap();
        let buffer = AllocationBuffer::from_external(
            request(),
            OwnedStorage::zeroed(request().minimum_bytes()).unwrap(),
        )
        .unwrap();
        let buffer = Mutex::new(Some(buffer));
        // Some platforms allocate a native mutex on its first lock. Initialize
        // this application policy's pool lock outside the measured callback;
        // register initializes the registry's own lock before SDK calls begin.
        drop(lock(&buffer));
        let token = registry
            .register(move |_| Ok(lock(&buffer).take().unwrap()))
            .unwrap();
        let (published, allocations) =
            crate::test_alloc::allocation_count(|| registry.allocate(token, request()));
        let (pointer, _) = published.unwrap();
        assert_eq!(allocations, 0);
        assert!(registry.free(token, pointer.as_ptr()));
    }

    #[test]
    fn lease_bookkeeping_is_fallibly_reserved_for_the_full_limit() {
        let limit = 32;
        let registry = Registry::new(AllocatorLimits {
            max_allocations: limit,
            max_bytes: limit * 64,
            ..limits()
        })
        .unwrap();
        let token = registry.register(AllocationBuffer::zeroed).unwrap();
        let capacity = lock(&registry.state).leases.capacity();
        assert!(capacity >= limit);
        let mut held = Vec::with_capacity(limit);
        for _ in 0..limit {
            let (published, allocations) =
                crate::test_alloc::allocation_count(|| registry.allocate(token, request()));
            held.push(published.unwrap().0);
            assert_eq!(allocations, 1);
        }
        let state = lock(&registry.state);
        assert_eq!(state.leases.len(), limit);
        assert_eq!(state.leases.capacity(), capacity);
        drop(state);
        for pointer in held {
            assert!(registry.free(token, pointer.as_ptr()));
        }

        // An impossible reservation is an allocation failure, rather than a panic or
        // an infallible allocation on the first SDK callback.
        assert!(matches!(
            Registry::new(AllocatorLimits {
                max_allocations: usize::MAX,
                ..limits()
            }),
            Err(Error::AllocationFailed(crate::AllocationFailure {
                kind: crate::AllocationKind::AllocatorLeases,
                bytes: None,
            }))
        ));
    }

    #[test]
    fn lease_churn_never_allocates_for_pool_storage() {
        let limit = 16;
        let changes = 1024;
        let registry = Registry::new(AllocatorLimits {
            max_allocations: limit,
            max_bytes: limit * 64,
            ..limits()
        })
        .unwrap();
        // All backing storage exists before tracking. FIFO claims visit distinct
        // addresses across churn, exercising the bounded map's tombstone rebuild.
        let pool = TestPool::new(limit + changes, 64);
        let policy_pool = pool.clone();
        let token = registry
            .register(move |request| policy_pool.buffer(request))
            .unwrap();
        let mut held = Vec::with_capacity(limit);
        for _ in 0..limit {
            held.push(registry.allocate(token, request()).unwrap().0);
        }
        for change in 0..changes {
            let slot = &mut held[change % limit];
            let (published, allocations) = crate::test_alloc::allocation_count(|| {
                assert!(registry.free(token, slot.as_ptr()));
                registry.allocate(token, request())
            });
            *slot = published.unwrap().0;
            assert_eq!(allocations, 0);
        }
        for pointer in held {
            assert!(registry.free(token, pointer.as_ptr()));
        }
        assert_eq!(pool.releases.load(Ordering::Relaxed), limit + changes);
    }

    #[test]
    fn unavailable_generations_and_policy_errors_have_distinct_decline_counts() {
        let registry = Registry::new(limits()).unwrap();
        let token = registry
            .register(|_: AllocationRequest| -> Result<AllocationBuffer> {
                Err(crate::error::allocation_failed(
                    crate::AllocationKind::AllocatorLeases,
                    None,
                ))
            })
            .unwrap();
        let (result, allocations) =
            crate::test_alloc::allocation_count(|| registry.allocate(token, request()));
        assert!(result.is_none());
        assert_eq!(allocations, 0);
        assert_eq!(registry.usage().declined(AllocationDecline::PolicyError), 1);
        registry.closing(token);
        assert!(registry.allocate(token, request()).is_none());
        registry.closing(token);
        assert!(registry.allocate(token, request()).is_none());
        let usage = registry.usage();
        assert_eq!(usage.declined(AllocationDecline::GenerationUnavailable), 2);
        assert_eq!(usage.declined_allocations(), 3);
        assert_eq!(usage.retained_bytes(), 0);
        assert_eq!(usage.in_progress_allocations(), 0);
    }

    #[test]
    fn panicking_external_buffer_inspection_counts_once_and_releases_storage() {
        use std::cell::Cell;

        struct InspectionPanics {
            storage: OwnedStorage,
            pointer_calls: Cell<usize>,
            capacity_calls: Cell<usize>,
            panic_in_pointer: bool,
            drops: Arc<AtomicUsize>,
        }
        // SAFETY: OwnedStorage supplies initialized, aligned, writable memory
        // with a stable pointer and capacity. This fixture panics on a later
        // accessor call without ever changing either returned value. Its
        // destructor is nonpanicking and releases the allocation exactly once.
        unsafe impl ExternalAllocation for InspectionPanics {
            fn pointer(&self) -> NonNull<u8> {
                let calls = self.pointer_calls.replace(self.pointer_calls.get() + 1);
                assert!(
                    !self.panic_in_pointer || calls == 0,
                    "buffer pointer inspection panicked"
                );
                self.storage.pointer()
            }

            fn capacity(&self) -> usize {
                let calls = self.capacity_calls.replace(self.capacity_calls.get() + 1);
                assert!(
                    self.panic_in_pointer || calls == 0,
                    "buffer capacity inspection panicked"
                );
                self.storage.capacity()
            }
        }
        impl Drop for InspectionPanics {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }

        for panic_in_pointer in [true, false] {
            let registry = Registry::new(limits()).unwrap();
            let drops = Arc::new(AtomicUsize::new(0));
            let token = registry
                .register({
                    let drops = drops.clone();
                    move |request: AllocationRequest| {
                        let storage = InspectionPanics {
                            storage: OwnedStorage::zeroed(request.minimum_bytes())?,
                            pointer_calls: Cell::new(0),
                            capacity_calls: Cell::new(0),
                            panic_in_pointer,
                            drops: drops.clone(),
                        };
                        Ok(AllocationBuffer::from_external(request, storage)?)
                    }
                })
                .unwrap();
            let outcome = catch_unwind(AssertUnwindSafe(|| registry.allocate(token, request())));
            assert!(
                outcome.is_ok(),
                "buffer inspection must remain inside panic containment"
            );
            assert!(outcome.unwrap().is_none());
            let usage = registry.usage();
            assert_eq!(usage.declined(AllocationDecline::PolicyPanic), 1);
            assert_eq!(usage.declined_allocations(), 1);
            assert_eq!(usage.retained_allocations(), 0);
            assert_eq!(usage.retained_bytes(), 0);
            assert_eq!(usage.in_progress_allocations(), 0);
            assert_eq!(lock(&registry.state).reserved_bytes, 0);
            assert_eq!(drops.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn declined_budget_requests_allocate_nothing_and_stay_exact_under_contention() {
        let registry = Registry::new(limits()).unwrap();
        let token = registry.register(AllocationBuffer::zeroed).unwrap();
        let oversized = request_of(128);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..32 {
                assert!(registry.allocate(token, oversized).is_none());
            }
        });
        assert_eq!(allocations, 0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..100 {
                        assert!(registry.allocate(token, oversized).is_none());
                        let usage = registry.usage();
                        assert_eq!(
                            usage.declined_allocations(),
                            usage.declined(AllocationDecline::ByteLimit)
                        );
                        assert_eq!(usage.in_progress_allocations(), 0);
                        assert_eq!(usage.retained_bytes(), 0);
                    }
                });
            }
        });
        assert_eq!(registry.usage().declined(AllocationDecline::ByteLimit), 832);
    }

    #[test]
    fn duplicate_live_addresses_are_declined_without_releasing_the_first_lease() {
        struct RepeatedAddress(Arc<Mutex<OwnedStorage>>);
        // SAFETY: this fixture never lets an SDK access the storage. It keeps
        // initialized aligned memory alive and deliberately offers its stable
        // address twice so the registry can refuse a second publication.
        unsafe impl ExternalAllocation for RepeatedAddress {
            fn pointer(&self) -> NonNull<u8> {
                lock(&self.0).pointer()
            }
            fn capacity(&self) -> usize {
                lock(&self.0).capacity()
            }
        }
        let registry = Registry::new(limits()).unwrap();
        let storage = Arc::new(Mutex::new(OwnedStorage::zeroed(64).unwrap()));
        let token = registry
            .register(move |request| {
                Ok(AllocationBuffer::from_external(
                    request,
                    RepeatedAddress(storage.clone()),
                )?)
            })
            .unwrap();
        let (pointer, _) = registry.allocate(token, request()).unwrap();
        assert!(registry.allocate(token, request()).is_none());
        let usage = registry.usage();
        assert_eq!(usage.declined(AllocationDecline::BufferRefused), 1);
        assert_eq!(usage.declined_allocations(), 1);
        assert_eq!(usage.retained_allocations(), 1);
        assert_eq!(usage.retained_bytes(), 64);
        assert_eq!(usage.in_progress_allocations(), 0);
        assert!(registry.free(token, pointer.as_ptr()));
        assert_eq!(registry.usage().retained_bytes(), 0);
    }

    #[test]
    fn invalid_buffer_layout_and_stride_overflow_are_counted_once() {
        for malformed in 0..3 {
            let registry = Registry::new(limits()).unwrap();
            let token = registry
                .register(move |request| {
                    let mut buffer = AllocationBuffer::zeroed(request)?;
                    // Simulate a malformed policy result inside the fixture;
                    // neither value can be published as a native descriptor.
                    match malformed {
                        0 => buffer.request.width += 1,
                        1 => buffer.request.stride = i32::MAX as usize + 1,
                        2 => buffer.request.bytes -= 1,
                        _ => unreachable!(),
                    }
                    Ok(buffer)
                })
                .unwrap();
            assert!(registry.allocate(token, request()).is_none());
            let usage = registry.usage();
            assert_eq!(usage.declined(AllocationDecline::BufferRefused), 1);
            assert_eq!(usage.declined_allocations(), 1);
            assert_eq!(usage.retained_allocations(), 0);
            assert_eq!(usage.retained_bytes(), 0);
            assert_eq!(usage.in_progress_allocations(), 0);
        }
    }

    fn compressed_requests(bytes: i32) -> [AllocationRequest; 2] {
        let mut video = NDIlib_video_frame_v2_t {
            FourCC: VideoCodec::H264.outer(StreamTier::Program, false),
            xres: 16,
            yres: 16,
            ..Default::default()
        };
        video.__bindgen_anon_1.data_size_in_bytes = bytes;
        let mut audio = NDIlib_audio_frame_v3_t {
            FourCC: AudioCodec::Opus.outer(false),
            sample_rate: 48_000,
            no_channels: 2,
            no_samples: 8,
            ..Default::default()
        };
        audio.__bindgen_anon_1.data_size_in_bytes = bytes;
        [
            video_request(&video).unwrap(),
            audio_request(&audio).unwrap(),
        ]
    }

    fn cached_request_policy() -> impl ReceiveAllocator {
        let cached = Mutex::new(None);
        move |request| AllocationBuffer::zeroed(*lock(&cached).get_or_insert(request))
    }

    #[test]
    fn stale_compressed_requests_are_declined_without_retaining_storage() {
        for (first, smaller) in compressed_requests(64)
            .into_iter()
            .zip(compressed_requests(32))
        {
            let registry = Registry::new(limits()).unwrap();
            let token = registry.register(cached_request_policy()).unwrap();
            let (pointer, size) = registry.allocate(token, first).unwrap();
            assert_eq!(size, 64);
            assert!(registry.free(token, pointer.as_ptr()));

            assert!(registry.allocate(token, smaller).is_none());
            let usage = registry.usage();
            assert_eq!(usage.declined(AllocationDecline::BufferRefused), 1);
            assert_eq!(usage.declined_allocations(), 1);
            assert_eq!(usage.retained_allocations(), 0);
            assert_eq!(usage.retained_bytes(), 0);
            assert_eq!(usage.in_progress_allocations(), 0);
        }
    }

    #[test]
    fn exact_compressed_sizes_and_valid_uncompressed_padding_are_published() {
        let video = video_request(&NDIlib_video_frame_v2_t {
            FourCC: FourCc::from(PixelFormat::Bgra).raw(),
            xres: 4,
            yres: 2,
            ..Default::default()
        })
        .unwrap();
        for request in compressed_requests(32)
            .into_iter()
            .chain([video, request_of(32)])
        {
            let registry = Registry::new(limits()).unwrap();
            let token = registry
                .register(|request: AllocationRequest| {
                    AllocationBuffer::zeroed(request.with_stride_bytes(32)?)
                })
                .unwrap();
            let (pointer, stride) = registry.allocate(token, request).unwrap();
            assert_eq!(stride, 32);
            assert_eq!(registry.usage().declined_allocations(), 0);
            assert!(registry.free(token, pointer.as_ptr()));
        }
    }

    #[cfg(ndi_has_advanced_receive_allocators)]
    #[test]
    fn compressed_callback_declines_clear_the_data_and_size() {
        // This is the only unit test using the process-wide registry. It drives
        // the native callback boundary directly without creating SDK objects.
        let registry = REGISTRY.get_or_init(|| Registry::new(limits()).unwrap());
        let token = registry.register(cached_request_policy()).unwrap();
        let opaque = token as *mut c_void;
        let mut video = NDIlib_video_frame_v2_t {
            FourCC: VideoCodec::H264.outer(StreamTier::Program, false),
            xres: 16,
            yres: 16,
            ..Default::default()
        };
        video.__bindgen_anon_1.data_size_in_bytes = 64;
        // SAFETY: this live descriptor belongs to this call, and the token
        // names a registered policy. The fixture performs no native access.
        unsafe {
            assert!(video_allocate(opaque, &mut video));
            assert!(video_free(opaque, &video));
            video.__bindgen_anon_1.data_size_in_bytes = 32;
            assert!(!video_allocate(opaque, &mut video));
            assert!(video.p_data.is_null());
            assert_eq!(video.__bindgen_anon_1.data_size_in_bytes, 0);
        }
        registry.closing(token);

        let token = registry.register(cached_request_policy()).unwrap();
        let opaque = token as *mut c_void;
        let mut audio = NDIlib_audio_frame_v3_t {
            FourCC: AudioCodec::Opus.outer(false),
            sample_rate: 48_000,
            no_channels: 2,
            no_samples: 8,
            ..Default::default()
        };
        audio.__bindgen_anon_1.data_size_in_bytes = 64;
        // SAFETY: as above, with the matching audio allocation and release.
        unsafe {
            assert!(audio_allocate(opaque, &mut audio));
            assert!(audio_free(opaque, &audio));
            audio.__bindgen_anon_1.data_size_in_bytes = 32;
            assert!(!audio_allocate(opaque, &mut audio));
            assert!(audio.p_data.is_null());
            assert_eq!(audio.__bindgen_anon_1.data_size_in_bytes, 0);
        }
        registry.closing(token);
        let usage = registry.usage();
        assert_eq!(usage.declined(AllocationDecline::BufferRefused), 2);
        assert_eq!(usage.retained_allocations(), 0);
        assert_eq!(usage.in_progress_allocations(), 0);
    }

    /// A policy that can be told apart from another of its type.
    struct Tagged(u32);

    impl ReceiveAllocator for Tagged {
        fn allocate(&self, request: AllocationRequest) -> Result<AllocationBuffer> {
            AllocationBuffer::zeroed(request)
        }
    }

    /// Where the policy calls of a test wait for each other.
    #[derive(Default)]
    struct Gate {
        state: Mutex<(usize, bool)>,
        changed: std::sync::Condvar,
    }

    impl Gate {
        const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

        /// Called by a policy: count the call, then wait to be let through.
        fn enter(&self) {
            let mut state = lock(&self.state);
            state.0 += 1;
            self.changed.notify_all();
            let _ = self
                .changed
                .wait_timeout_while(state, Self::PATIENCE, |state| !state.1);
        }

        /// Wait until `calls` policy calls are inside, then say whether they
        /// all arrived.
        fn wait_for(&self, calls: usize) -> bool {
            let (state, _) = self
                .changed
                .wait_timeout_while(lock(&self.state), Self::PATIENCE, |state| state.0 < calls)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.0 >= calls
        }

        fn open(&self) {
            lock(&self.state).1 = true;
            self.changed.notify_all();
        }
    }

    #[test]
    fn limits_are_accepted_only_when_they_can_all_be_reached() {
        let defaults = AllocatorLimits::default().validate().unwrap();
        assert_eq!(
            (
                defaults.max_generations,
                defaults.max_allocations,
                defaults.max_bytes,
                defaults.max_concurrent_calls,
                defaults.max_allocation_bytes,
            ),
            (256, 1024, 1024 * MIB, 16, 64 * MIB)
        );

        let reason = |limits: AllocatorLimits| match limits.validate() {
            Err(crate::Error::InvalidInput(reason)) => reason,
            other => panic!("expected the limits to be refused, got {other:?}"),
        };
        for zero in [
            defaults.max_generations(0),
            defaults.max_allocations(0),
            defaults.max_concurrent_calls(0),
            defaults.max_allocation_bytes(0),
        ] {
            assert!(matches!(
                reason(zero),
                crate::InputDefect::AllocatorPositiveLimits
            ));
        }
        assert!(matches!(
            reason(defaults.max_allocation_bytes(MAX_ALLOCATION + 1)),
            crate::InputDefect::AllocatorAllocationLimit
        ));
        assert!(matches!(
            reason(defaults.max_allocations(15)),
            crate::InputDefect::AllocatorConcurrentLimit
        ));
        // Each of these promises more concurrent calls than the byte budget
        // can grant a largest allocation to.
        assert!(matches!(
            reason(defaults.max_concurrent_calls(17)),
            crate::InputDefect::AllocatorByteLimit
        ));
        assert!(matches!(
            reason(defaults.max_allocation_bytes(64 * MIB + 1)),
            crate::InputDefect::AllocatorByteLimit
        ));
        assert!(matches!(
            reason(defaults.max_bytes(1024 * MIB - 1)),
            crate::InputDefect::AllocatorByteLimit
        ));
        assert!(matches!(
            reason(
                defaults
                    .max_concurrent_calls(32)
                    .max_allocation_bytes(MAX_ALLOCATION)
            ),
            crate::InputDefect::AllocatorByteLimit
        ));
        assert!(matches!(
            reason(
                defaults
                    .max_bytes(usize::MAX)
                    .max_allocations(usize::MAX)
                    .max_concurrent_calls(usize::MAX)
            ),
            crate::InputDefect::AllocatorByteLimit
        ));

        // The largest single allocation, with the budget to match: accepted
        // whenever the budget for the calls can be named. On a 32-bit target
        // sixteen largest allocations are 4 GiB, which no `usize` holds, so
        // the same limits are refused there as unreachable.
        for calls in [4, 16] {
            let limits = defaults
                .max_allocation_bytes(MAX_ALLOCATION)
                .max_concurrent_calls(calls);
            match calls.checked_mul(MAX_ALLOCATION) {
                Some(budget) => {
                    limits.max_bytes(budget).validate().unwrap();
                }
                None => assert!(matches!(
                    reason(limits.max_bytes(usize::MAX)),
                    crate::InputDefect::AllocatorByteLimit
                )),
            }
        }
        defaults
            .max_allocation_bytes(MAX_ALLOCATION)
            .max_concurrent_calls(4)
            .validate()
            .unwrap();
    }

    /// With the default limits, as many policy calls as the limits name run
    /// at the same time, each granted a largest allocation, and one more is
    /// declined.
    #[test]
    fn every_concurrent_call_the_limits_name_is_admitted() {
        let limits = AllocatorLimits::default();
        let calls = limits.max_concurrent_calls;
        let registry = Registry::new(limits).unwrap();
        let gate = Arc::new(Gate::default());
        let granted = Arc::new(Mutex::new(Vec::new()));
        let token = registry
            .register({
                let (gate, granted) = (gate.clone(), granted.clone());
                move |request: AllocationRequest| {
                    lock(&granted).push(request.capacity_limit());
                    gate.enter();
                    AllocationBuffer::zeroed(request)
                }
            })
            .unwrap();

        let (admitted, all_inside, in_progress, one_more) = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..calls)
                .map(|_| {
                    scope.spawn(|| {
                        let (pointer, _) = registry.allocate(token, request())?;
                        Some(pointer.as_ptr() as usize)
                    })
                })
                .collect();
            let all_inside = gate.wait_for(calls);
            let in_progress = lock(&registry.state).calls;
            let one_more = registry.allocate(token, request());
            gate.open();
            let admitted: Vec<_> = workers
                .into_iter()
                .filter_map(|worker| worker.join().unwrap())
                .collect();
            (admitted, all_inside, in_progress, one_more)
        });
        assert!(
            all_inside,
            "only {in_progress} of {calls} calls ran at once"
        );
        assert_eq!(in_progress, calls);
        assert!(one_more.is_none(), "a call beyond the limit was admitted");
        let usage = registry.usage();
        assert_eq!(usage.declined(AllocationDecline::ConcurrencyLimit), 1);
        assert_eq!(usage.declined_allocations(), 1);
        assert_eq!(admitted.len(), calls);
        assert_eq!(*lock(&granted), vec![limits.max_allocation_bytes; calls]);

        let state = lock(&registry.state);
        assert_eq!((state.calls, state.reserved_bytes), (0, 0));
        assert_eq!((state.leases.len(), state.bytes), (calls, calls * 64));
        drop(state);
        for address in admitted {
            assert!(registry.free(token, address as *mut u8));
        }
        registry.closing(token);
    }

    /// A call is granted what is left of the byte budget, and declined only
    /// when that cannot hold the frame; buffers the SDK already holds do not
    /// cost a later call more than their own size.
    #[test]
    fn a_call_is_granted_what_is_left_of_the_byte_budget() {
        let registry = Registry::new(
            AllocatorLimits {
                max_generations: 1,
                max_allocations: 8,
                max_bytes: 256,
                max_concurrent_calls: 4,
                max_allocation_bytes: 64,
            }
            .validate()
            .unwrap(),
        )
        .unwrap();
        let granted = Arc::new(Mutex::new(Vec::new()));
        let token = registry
            .register({
                let granted = granted.clone();
                move |request: AllocationRequest| {
                    lock(&granted).push(request.capacity_limit());
                    AllocationBuffer::zeroed(request)
                }
            })
            .unwrap();
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(registry.allocate(token, request()).unwrap().0);
        }
        held.push(registry.allocate(token, request_of(32)).unwrap().0);
        // 224 of 256 bytes are held: 32 are left.
        assert!(registry.allocate(token, request()).is_none());
        held.push(registry.allocate(token, request_of(16)).unwrap().0);
        assert!(registry.allocate(token, request_of(32)).is_none());
        held.push(registry.allocate(token, request_of(16)).unwrap().0);
        assert!(registry.allocate(token, request_of(8)).is_none());
        assert_eq!(*lock(&granted), [64, 64, 64, 64, 32, 16]);
        assert_eq!(lock(&registry.state).bytes, 256);
        assert_eq!(registry.usage().declined(AllocationDecline::ByteLimit), 3);
        assert_eq!(registry.usage().declined_allocations(), 3);

        // Freeing returns exactly what the buffer took.
        assert!(registry.free(token, held[0].as_ptr()));
        assert!(registry.allocate(token, request()).is_some());
        assert_eq!(lock(&granted).last(), Some(&64));
    }

    #[test]
    fn a_buffer_beyond_its_grant_is_declined_and_nothing_stays_charged() {
        let registry = Registry::new(limits()).unwrap();
        let token = registry
            .register(|request: AllocationRequest| {
                // Twice what the call was granted.
                let padded = AllocationRequest {
                    capacity_limit: MAX_ALLOCATION,
                    ..request
                };
                AllocationBuffer::zeroed(padded.with_stride_bytes(request.stride_bytes() * 4)?)
            })
            .unwrap();
        assert!(registry.allocate(token, request()).is_none());
        assert_eq!(
            registry.usage().declined(AllocationDecline::BufferRefused),
            1
        );
        assert_eq!(registry.usage().declined_allocations(), 1);
        let state = lock(&registry.state);
        assert_eq!((state.calls, state.reserved_bytes, state.bytes), (0, 0, 0));
        assert!(state.leases.is_empty());
    }

    #[test]
    fn a_counter_advances_only_while_its_rule_allows() {
        let counter = AtomicUsize::new(0);
        let below = |limit| move |value| (value < limit).then_some(value + 1);
        assert_eq!(advance(&counter, below(2)), Some(0));
        assert_eq!(advance(&counter, below(2)), Some(1));
        assert_eq!(advance(&counter, below(2)), None);
        assert_eq!(counter.load(Ordering::Acquire), 2);

        counter.store(usize::MAX - 1, Ordering::Release);
        let next = |value: usize| value.checked_add(1);
        assert_eq!(advance(&counter, next), Some(usize::MAX - 1));
        assert_eq!(advance(&counter, next), None);
        assert_eq!(counter.load(Ordering::Acquire), usize::MAX);

        // Racing threads take exactly the room there is.
        let counter = AtomicUsize::new(0);
        let taken = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        if advance(&counter, below(1000)).is_some() {
                            taken.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(counter.load(Ordering::Acquire), 1000);
        assert_eq!(taken.load(Ordering::Relaxed), 1000);
    }

    #[test]
    fn a_policy_the_process_has_no_room_for_is_handed_back() {
        let registry = Registry::new(limits()).unwrap();
        let first = registry.register(Tagged(1)).unwrap();
        let second = registry.register(Tagged(2)).unwrap();
        assert_eq!((first, second), (1, 2));

        // The generation budget is exhausted.
        let rejected = registry.register(Tagged(3)).unwrap_err();
        assert!(
            matches!(rejected.error(), Error::InvalidInput(reason) if matches!(reason, crate::InputDefect::AllocatorGenerationBudget))
        );
        assert_eq!(rejected.into_inner().0, 3);
        assert_eq!(registry.generations.load(Ordering::Acquire), 2);
        assert_eq!(lock(&registry.state).active.len(), 2);

        // Room again, but no token left to name a generation with: the
        // generation that was counted for the attempt is given up.
        registry.closing(second);
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        registry.next.store(usize::MAX, Ordering::Release);
        let rejected = registry.register(Tagged(4)).unwrap_err();
        assert!(matches!(
            rejected.error(),
            Error::InvalidInput(crate::InputDefect::AllocatorTokensExhausted)
        ));
        assert_eq!(rejected.into_inner().0, 4);
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        assert_eq!(registry.next.load(Ordering::Acquire), usize::MAX);

        // The last token is still handed out.
        registry.next.store(usize::MAX - 1, Ordering::Release);
        assert_eq!(registry.register(Tagged(5)).unwrap(), usize::MAX - 1);
        assert!(registry.allocate(usize::MAX - 1, request()).is_some());
    }

    #[test]
    fn memory_a_request_cannot_use_is_handed_back() {
        /// Storage that reports an address one byte into its allocation.
        struct Misaligned(OwnedStorage);
        // SAFETY: the buffer is refused for its alignment before any use.
        unsafe impl ExternalAllocation for Misaligned {
            fn pointer(&self) -> NonNull<u8> {
                // SAFETY: the allocation is longer than one byte.
                unsafe { self.0.pointer().add(1) }
            }
            fn capacity(&self) -> usize {
                self.0.capacity() - 1
            }
        }

        let request = AllocationRequest {
            capacity_limit: 128,
            ..request()
        };
        let refused = |storage: OwnedStorage, expected: crate::InputDefect| {
            let address = storage.pointer();
            let rejected = AllocationBuffer::from_external(request, storage)
                .expect_err("the storage does not fit the request");
            assert!(matches!(rejected.error(), Error::InvalidInput(reason) if *reason==expected));
            assert_eq!(rejected.into_inner().pointer(), address);
        };
        refused(
            OwnedStorage::zeroed(32).unwrap(),
            crate::InputDefect::ExternalAllocationTooSmall,
        );
        refused(
            OwnedStorage::zeroed(192).unwrap(),
            crate::InputDefect::ExternalAllocationTooLarge,
        );
        let rejected = AllocationBuffer::from_external(
            request,
            Misaligned(OwnedStorage::zeroed(128).unwrap()),
        )
        .expect_err("the storage is misaligned");
        assert!(matches!(
            rejected.error(),
            Error::InvalidInput(crate::InputDefect::ExternalAllocationAlignment)
        ));
        assert_eq!(rejected.into_inner().0.capacity(), 128);

        let buffer =
            AllocationBuffer::from_external(request, OwnedStorage::zeroed(128).unwrap()).unwrap();
        assert_eq!(buffer.capacity(), 128);
        assert!(OwnedStorage::zeroed(0).is_err());
    }

    #[test]
    fn padded_audio_cannot_publish_a_layout_capture_would_refuse() {
        const MIB: usize = 1024 * 1024;
        let registry = Registry::new(
            AllocatorLimits::default()
                .max_allocation_bytes(256 * MIB)
                .max_concurrent_calls(4)
                .max_bytes(1024 * MIB)
                .validate()
                .unwrap(),
        )
        .unwrap();
        let format = AudioFormat::Fltp;
        let request = audio_request(&NDIlib_audio_frame_v3_t {
            FourCC: format.to_sdk(),
            sample_rate: 48_000,
            no_channels: 2,
            no_samples: 1024,
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(
            request.with_stride_bytes(4098),
            Err(Error::InvalidInput(crate::InputDefect::Layout(
                crate::LayoutDefect::AudioStrideAlignment(4098)
            )))
        ));
        assert!(matches!(
            request.with_stride_bytes(65 * MIB),
            Err(Error::InvalidInput(crate::InputDefect::Layout(
                crate::LayoutDefect::AudioBytes(bytes)
            ))) if bytes.get() == (65 * MIB + 4096) as u64
        ));
        let token = registry
            .register(|request: AllocationRequest| {
                AllocationBuffer::zeroed(request.with_stride_bytes(65 * MIB)?)
            })
            .unwrap();
        assert!(registry.allocate(token, request).is_none());
        assert_eq!(registry.usage().declined(AllocationDecline::PolicyError), 1);
        let state = lock(&registry.state);
        assert_eq!((state.calls, state.reserved_bytes, state.bytes), (0, 0, 0));
        assert!(state.leases.is_empty());
    }

    #[test]
    fn strides_a_request_cannot_use_are_refused_as_input() {
        let input = |result: Result<AllocationRequest>| match result {
            Err(Error::InvalidInput(reason)) => reason,
            other => panic!("expected invalid input, got {other:?}"),
        };
        let planar = video_request(&NDIlib_video_frame_v2_t {
            FourCC: FourCc::from(PixelFormat::I420).raw(),
            xres: 16,
            yres: 16,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(planar.stride_bytes(), 16);
        let padded = planar.with_stride_bytes(32).unwrap();
        assert_eq!(
            (padded.stride_bytes(), padded.minimum_bytes()),
            (32, 32 * 16 * 3 / 2)
        );
        assert!(matches!(
            input(planar.with_stride_bytes(15)),
            crate::InputDefect::AllocatorShortStride { .. }
        ));
        let crate::InputDefect::Layout(input_cause) = input(planar.with_stride_bytes(17)) else {
            panic!("expected the shared layout cause")
        };
        let Err(Error::InvalidFrame(crate::FrameDefect::Layout(frame_cause))) =
            PixelFormat::I420.send_buffer_len(16, 16, Some(17), crate::ScanType::Progressive)
        else {
            panic!("expected the same frame layout refusal")
        };
        assert_eq!(input_cause, frame_cause);

        let audio = request();
        for stride in [0, 30, 34, i32::MAX as usize + 1] {
            let crate::InputDefect::Layout(input_cause) = input(audio.with_stride_bytes(stride))
            else {
                panic!("expected the shared audio layout cause");
            };
            let Err(Error::InvalidFrame(crate::FrameDefect::Layout(frame_cause))) =
                ValidatedAudioLayout::new_uncompressed(
                    AudioFormat::Fltp,
                    48_000,
                    2,
                    8,
                    Some(stride),
                )
            else {
                panic!("expected the same frame layout refusal");
            };
            assert_eq!(input_cause, frame_cause);
        }
        let limited = AllocationRequest {
            capacity_limit: 64,
            ..audio
        };
        assert!(matches!(
            input(limited.with_stride_bytes(36)),
            crate::InputDefect::AllocationTooLarge { .. }
        ));

        let compressed = AllocationRequest {
            format: AllocationFormat::CompressedAudio(FourCc::from_raw(
                AudioCodec::Opus.outer(false),
            )),
            audio_layout: None,
            ..audio
        };
        assert_eq!(compressed.with_stride_bytes(32).unwrap(), compressed);
        assert!(matches!(
            input(compressed.with_stride_bytes(36)),
            crate::InputDefect::CompressedAllocationStride
        ));
    }

    #[test]
    fn retired_lease_keeps_generation_and_budget_until_matching_free() {
        let registry = Registry::new(limits()).unwrap();
        let token = registry.register(AllocationBuffer::zeroed).unwrap();
        let (pointer, _) = registry.allocate(token, request()).unwrap();
        registry.closing(token);
        assert!(registry.allocate(token, request()).is_none());
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        let next = registry.register(AllocationBuffer::zeroed).unwrap();
        assert_ne!(token, next);
        assert!(registry.register(AllocationBuffer::zeroed).is_err());
        assert!(registry.free(token, pointer.as_ptr()));
        assert!(!registry.free(token, pointer.as_ptr()));
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        registry.closing(next);
    }

    #[test]
    fn a_free_releases_only_an_allocation_of_the_generation_it_names() {
        let registry = Registry::new(limits()).unwrap();
        let first = registry.register(AllocationBuffer::zeroed).unwrap();
        let (pointer, _) = registry.allocate(first, request()).unwrap();
        // The receiver moves on to a second generation while the SDK still
        // holds the first one's buffer.
        registry.closing(first);
        let second = registry.register(AllocationBuffer::zeroed).unwrap();

        // A free that names the second generation cannot release it.
        assert!(!registry.free(second, pointer.as_ptr()));
        assert_eq!(lock(&registry.state).bytes, 64);
        assert_eq!(registry.generations.load(Ordering::Acquire), 2);

        // The free the SDK sends names the generation that allocated, and
        // releases the buffer, its budget and the retired generation.
        assert!(registry.free(first, pointer.as_ptr()));
        assert_eq!(lock(&registry.state).bytes, 0);
        assert!(lock(&registry.state).leases.is_empty());
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        registry.closing(second);
        assert_eq!(registry.generations.load(Ordering::Acquire), 0);
    }

    #[test]
    fn replacement_is_installed_before_the_earlier_policy_is_destroyed() {
        struct ObserveDrop {
            setter_calls: Arc<AtomicUsize>,
            observed: Arc<AtomicUsize>,
        }
        impl Drop for ObserveDrop {
            fn drop(&mut self) {
                self.observed
                    .store(self.setter_calls.load(Ordering::Acquire), Ordering::Release);
            }
        }

        // Resetting to SDK memory uses the same ordering as replacement.
        for reset in [false, true] {
            let registry = Registry::new(limits()).unwrap();
            let setter_calls = Arc::new(AtomicUsize::new(1));
            let observed = Arc::new(AtomicUsize::new(0));
            let observer = ObserveDrop {
                setter_calls: setter_calls.clone(),
                observed: observed.clone(),
            };
            let earlier = registry
                .register(move |request| {
                    let _ = &observer;
                    AllocationBuffer::zeroed(request)
                })
                .unwrap();
            let replacement =
                (!reset).then(|| registry.register(AllocationBuffer::zeroed).unwrap());
            let mut slot = Some(earlier);
            install_with(&registry, &mut slot, replacement, || {
                setter_calls.fetch_add(1, Ordering::AcqRel);
                assert_eq!(observed.load(Ordering::Acquire), 0);
            });
            assert_eq!(slot, replacement);
            assert_eq!(observed.load(Ordering::Acquire), 2);
            if let Some(replacement) = replacement {
                registry.closing(replacement);
            }
        }
    }

    #[test]
    fn setter_allocations_publish_but_calls_still_running_at_retirement_do_not() {
        let registry = Registry::new(limits()).unwrap();
        let gate = Arc::new(Gate::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let earlier = registry
            .register({
                let gate = gate.clone();
                move |request| {
                    if calls.fetch_add(1, Ordering::AcqRel) == 0 {
                        gate.enter();
                    }
                    AllocationBuffer::zeroed(request)
                }
            })
            .unwrap();
        let replacement = registry.register(AllocationBuffer::zeroed).unwrap();
        let mut slot = Some(earlier);
        std::thread::scope(|scope| {
            let unfinished = scope.spawn(|| registry.allocate(earlier, request()).is_some());
            assert!(gate.wait_for(1));
            let mut published = None;
            let setter_calls = AtomicUsize::new(0);
            install_with(&registry, &mut slot, Some(replacement), || {
                setter_calls.fetch_add(1, Ordering::AcqRel);
                // The native setter has run, but still has the earlier
                // callbacks available until it returns.
                published = registry.allocate(earlier, request());
                assert!(published.is_some());
            });
            assert_eq!(setter_calls.load(Ordering::Acquire), 1);
            assert_eq!(slot, Some(replacement));
            assert_eq!(registry.usage().active_generations(), 1);
            gate.open();
            assert!(!unfinished.join().unwrap());
            assert_eq!(
                registry
                    .usage()
                    .declined(AllocationDecline::GenerationUnavailable),
                1
            );
            assert!(registry.free(earlier, published.unwrap().0.as_ptr()));
        });
        registry.closing(replacement);
        assert_eq!(registry.usage().generations(), 0);
    }

    #[test]
    fn policy_cannot_publish_after_close() {
        let registry = Registry::new(limits()).unwrap();
        let gate = Arc::new(Gate::default());
        let token = registry
            .register({
                let gate = gate.clone();
                move |request| {
                    gate.enter();
                    AllocationBuffer::zeroed(request)
                }
            })
            .unwrap();
        let published = std::thread::scope(|scope| {
            let worker = scope.spawn(|| registry.allocate(token, request()).is_some());
            assert!(gate.wait_for(1));
            registry.closing(token);
            assert_eq!(registry.usage().active_generations(), 0);
            assert_eq!(registry.usage().in_progress_allocations(), 1);
            assert_eq!(registry.usage().generations(), 1);
            let replacement = registry.register(AllocationBuffer::zeroed).unwrap();
            let (pointer, _) = registry.allocate(replacement, request()).unwrap();
            assert!(registry.free(replacement, pointer.as_ptr()));
            registry.closing(replacement);
            gate.open();
            worker.join().unwrap()
        });
        assert!(!published);
        assert_eq!(
            registry
                .usage()
                .declined(AllocationDecline::GenerationUnavailable),
            1
        );
        assert_eq!(registry.usage().declined_allocations(), 1);
        assert_eq!(lock(&registry.state).bytes, 0);
        assert_eq!(registry.generations.load(Ordering::Acquire), 0);
    }

    #[test]
    fn budgets_cover_multiple_generations_and_panic_paths() {
        let registry = Registry::new(limits()).unwrap();
        let first = registry.register(AllocationBuffer::zeroed).unwrap();
        let second = registry.register(AllocationBuffer::zeroed).unwrap();
        let (a, _) = registry.allocate(first, request()).unwrap();
        let (b, _) = registry.allocate(second, request()).unwrap();
        assert!(registry.allocate(first, request()).is_none());
        assert_eq!(
            registry
                .usage()
                .declined(AllocationDecline::AllocationCountLimit),
            1
        );
        registry.closing(first);
        registry.closing(second);
        assert!(registry.free(first, a.as_ptr()));
        assert!(registry.free(second, b.as_ptr()));
        let panic = registry
            .register(|_| -> Result<AllocationBuffer> { panic!("policy") })
            .unwrap();
        assert!(registry.allocate(panic, request()).is_none());
        assert_eq!(registry.usage().declined(AllocationDecline::PolicyPanic), 1);
        assert_eq!(registry.usage().declined_allocations(), 2);
        assert_eq!(lock(&registry.state).calls, 0);
        assert_eq!(lock(&registry.state).reserved_bytes, 0);
        registry.closing(panic);
    }

    #[test]
    fn blocked_storage_reclamation_remains_charged() {
        struct DelayedStorage {
            storage: OwnedStorage,
            gate: Arc<Gate>,
        }
        // SAFETY: the storage is an owned allocation that this value neither
        // changes nor shares.
        unsafe impl ExternalAllocation for DelayedStorage {
            fn pointer(&self) -> NonNull<u8> {
                self.storage.pointer()
            }
            fn capacity(&self) -> usize {
                self.storage.capacity()
            }
        }
        impl Drop for DelayedStorage {
            fn drop(&mut self) {
                self.gate.enter();
            }
        }
        let registry = Registry::new(
            AllocatorLimits {
                max_allocations: 1,
                max_concurrent_calls: 1,
                ..limits()
            }
            .validate()
            .unwrap(),
        )
        .unwrap();
        let gate = Arc::new(Gate::default());
        let token = registry
            .register({
                let gate = gate.clone();
                move |request: AllocationRequest| {
                    let storage = DelayedStorage {
                        storage: OwnedStorage::zeroed(request.minimum_bytes())?,
                        gate: gate.clone(),
                    };
                    Ok(AllocationBuffer::from_external(request, storage)?)
                }
            })
            .unwrap();
        let (pointer, _) = registry.allocate(token, request()).unwrap();
        let pointer = pointer.as_ptr() as usize;
        std::thread::scope(|scope| {
            let free = scope.spawn(|| registry.free(token, pointer as *mut u8));
            assert!(gate.wait_for(1));
            assert_eq!(lock(&registry.state).bytes, 64);
            assert_eq!(lock(&registry.state).reclaiming, 1);
            assert!(registry.allocate(token, request()).is_none());
            gate.open();
            assert!(free.join().unwrap());
        });
        assert_eq!(lock(&registry.state).bytes, 0);
        registry.closing(token);
    }
}
