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
    ffi::c_void,
    mem::ManuallyDrop,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::NonNull,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard, OnceLock,
    },
};

use super::{
    invalid_frame, invalid_input, sdk_quantity, AudioCodec, PacketEvent, PacketReceiver,
    ReceiverControls, VideoCodec,
};
use crate::{
    frames::ValidatedVideoLayout,
    receiver::PacketAttached,
    runtime::ProcessLifetimeRuntime,
    sys::{
        advanced::{native, ReceiveAllocators},
        bindings::*,
    },
    Error, FourCc, PixelFormat, RawCapture, Receiver, ReceiverMode, Rejected, Result,
};

const MIB: usize = 1024 * 1024;

const MAX_ALLOCATION: usize = 256 * MIB;

/// Process-wide limits on custom receive allocation.
///
/// The limits cover every receiver in the process, including allocations made
/// by policies that have since been replaced and buffers whose release is
/// still in progress. Memory a policy allocates for its own bookkeeping is not
/// counted. A request the limits do not admit is declined, and the SDK then
/// drops that frame.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        let limits_error = |reason: &str| crate::Error::InvalidInput(reason.into());
        if self.max_generations == 0
            || self.max_allocations == 0
            || self.max_concurrent_calls == 0
            || self.max_allocation_bytes == 0
        {
            return Err(limits_error("Allocator limits must be positive"));
        }
        if self.max_allocation_bytes > MAX_ALLOCATION {
            return Err(limits_error("max_allocation_bytes exceeds 256 MiB"));
        }
        if self.max_concurrent_calls > self.max_allocations {
            return Err(limits_error(
                "max_concurrent_calls exceeds max_allocations, which counts every call in progress",
            ));
        }
        if self
            .max_concurrent_calls
            .checked_mul(self.max_allocation_bytes)
            .is_none_or(|granted| granted > self.max_bytes)
        {
            return Err(limits_error(
                "max_bytes is less than max_concurrent_calls allocations of max_allocation_bytes",
            ));
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
    /// [`Error::InvalidInput`] when the stride is smaller than requested, is
    /// not one the frame's layout allows (an audio stride is a multiple of
    /// four bytes; a 4:2:0 or UYVA video stride is even), the frame is
    /// compressed (it has no stride to change), or the buffer would exceed
    /// the [`capacity_limit`](Self::capacity_limit).
    pub fn with_stride_bytes(self, stride: usize) -> Result<Self> {
        if stride < self.stride {
            return Err(invalid_input(format!(
                "Allocator stride {stride} is smaller than the requested stride {}",
                self.stride
            )));
        }
        let bytes = match self.format {
            AllocationFormat::Video(pixel) => {
                ValidatedVideoLayout::new_uncompressed(pixel, self.width, self.height, Some(stride))
                    // The layout rules report the frame that breaks them;
                    // here what breaks them is the caller's stride.
                    .map_err(|error| match error {
                        Error::InvalidFrame(reason) => invalid_input(reason),
                        other => other,
                    })?
                    .data_len_bytes
            }
            AllocationFormat::AudioPlanarF32 => {
                if !stride.is_multiple_of(size_of::<f32>()) {
                    return Err(invalid_input(format!(
                        "Audio allocator stride {stride} is not a whole number of samples"
                    )));
                }
                stride
                    .checked_mul(self.height as usize)
                    .ok_or_else(|| invalid_input("Audio allocator extent overflow"))?
            }
            _ if stride == self.stride => self.bytes,
            _ => {
                return Err(invalid_input(
                    "Compressed allocations have a byte size, not a configurable stride",
                ))
            }
        };
        if bytes > self.capacity_limit {
            return Err(invalid_input(format!(
                "An allocation of {bytes} bytes exceeds the capacity limit of {} bytes",
                self.capacity_limit
            )));
        }
        Ok(Self {
            bytes,
            stride,
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
            .ok_or_else(|| invalid_input(format!("No allocation can hold {bytes} bytes")))?;
        // SAFETY: the layout was just checked not to be zero-sized.
        let pointer = NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or_else(|| {
            invalid_input(format!("The global allocator did not supply {bytes} bytes"))
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
    storage: Box<dyn ExternalAllocation>,
    request: AllocationRequest,
}

impl AllocationBuffer {
    /// Allocate zeroed, 64-byte-aligned memory of the request's minimum size.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the global allocator cannot supply that
    /// much memory.
    pub fn zeroed(request: AllocationRequest) -> Result<Self> {
        Ok(Self {
            storage: Box::new(OwnedStorage::zeroed(request.bytes)?),
            request,
        })
    }

    /// Lend memory from the application's own pool.
    ///
    /// # Errors
    ///
    /// Returns the memory with [`Error::InvalidInput`] when it is smaller than
    /// the request's minimum, larger than its capacity limit, or not aligned
    /// to 64 bytes, so that a pool gets its buffer back.
    pub fn from_external<S: ExternalAllocation>(
        request: AllocationRequest,
        storage: S,
    ) -> std::result::Result<Self, Rejected<S>> {
        let capacity = storage.capacity();
        let problem = if capacity < request.bytes {
            Some("External allocation is smaller than the request's minimum")
        } else if capacity > request.capacity_limit {
            Some("External allocation is larger than the request's capacity limit")
        } else if !(storage.pointer().as_ptr() as usize).is_multiple_of(64) {
            Some("External allocation is not aligned to 64 bytes")
        } else {
            None
        };
        match problem {
            Some(problem) => Err(Rejected::new(invalid_input(problem), storage)),
            None => Ok(Self {
                storage: Box::new(storage),
                request,
            }),
        }
    }

    /// The size of the memory in bytes.
    pub fn capacity(&self) -> usize {
        self.storage.capacity()
    }
}

/// A policy that supplies the memory received frames are written into.
///
/// Any closure `Fn(AllocationRequest) -> Result<AllocationBuffer>` that is
/// `Send + Sync` is a policy.
///
/// The SDK calls the policy on its own threads, possibly several at once, and
/// stalls video or audio while a call blocks. An error or a panic makes that
/// one allocation fail; the SDK then drops the frame.
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

/// Where a generation is in its life.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Registered with a receiver: it may allocate.
    Active,
    /// Being replaced: a call already inside the policy may not publish.
    Closing,
    /// Replaced: only its outstanding leases keep it alive.
    Retired,
}

/// One registered policy.
struct Generation {
    phase: Mutex<Phase>,
    policy: ManuallyDrop<Arc<dyn ReceiveAllocator>>,
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
    bytes: usize,
    _generation: Arc<Generation>,
}

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
    leases: HashMap<(usize, usize), Lease>,
    bytes: usize,
    reserved_bytes: usize,
    calls: usize,
    reclaiming: usize,
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
struct Reservation<'a> {
    registry: &'a Registry,
    bytes: usize,
}

impl Reservation<'_> {
    /// End the call under a lock the caller already holds, so that the
    /// buffer it publishes is never charged beside its own grant.
    fn release(self, state: &mut State) {
        state.calls -= 1;
        state.reserved_bytes -= self.bytes;
        std::mem::forget(self);
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.registry.state);
        state.calls -= 1;
        state.reserved_bytes -= self.bytes;
    }
}

impl Registry {
    fn new(limits: AllocatorLimits) -> Self {
        Self {
            limits,
            state: Mutex::new(State::default()),
            generations: Arc::new(AtomicUsize::new(0)),
            next: AtomicUsize::new(1),
        }
    }

    /// Register `policy` as a new generation and return its token, or return
    /// the policy when the process has no room for another.
    fn register<P: ReceiveAllocator>(&self, policy: P) -> std::result::Result<usize, Rejected<P>> {
        let token = match self.admit() {
            Ok(token) => token,
            Err(error) => return Err(Rejected::new(error, policy)),
        };
        lock(&self.state).active.insert(
            token,
            Arc::new(Generation {
                phase: Mutex::new(Phase::Active),
                policy: ManuallyDrop::new(Arc::new(policy)),
                count: self.generations.clone(),
            }),
        );
        Ok(token)
    }

    /// Count one more generation and name it with a token that has never
    /// been used.
    fn admit(&self) -> Result<usize> {
        advance(&self.generations, |count| {
            (count < self.limits.max_generations).then_some(count + 1)
        })
        .ok_or_else(|| invalid_input("Process allocator generation budget exhausted"))?;
        advance(&self.next, |token| token.checked_add(1)).ok_or_else(|| {
            self.generations.fetch_sub(1, Ordering::AcqRel);
            invalid_input("Allocator token space exhausted")
        })
    }

    fn closing(&self, token: usize) {
        let generation = lock(&self.state).active.get(&token).cloned();
        if let Some(g) = generation {
            *lock(&g.phase) = Phase::Closing;
        }
    }

    fn retire(&self, token: usize) {
        self.closing(token);
        let generation = lock(&self.state).active.remove(&token);
        if let Some(g) = generation {
            *lock(&g.phase) = Phase::Retired;
            drop(g);
        }
    }

    fn allocate(&self, token: usize, mut request: AllocationRequest) -> Option<(NonNull<u8>, i32)> {
        let (generation, reservation) = {
            let mut s = lock(&self.state);
            let generation = s.active.get(&token)?.clone();
            if s.calls >= self.limits.max_concurrent_calls
                || s.leases.len() + s.calls + s.reclaiming >= self.limits.max_allocations
            {
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
        if *lock(&generation.phase) != Phase::Active {
            return None;
        }
        let allocation = catch_unwind(AssertUnwindSafe(|| generation.policy.allocate(request)))
            .ok()?
            .ok()?;
        let pointer = allocation.storage.pointer();
        let capacity = allocation.capacity();
        let actual = allocation.request;
        if capacity > reservation.bytes
            || actual.format != request.format
            || actual.width != request.width
            || actual.height != request.height
            || actual.stride < request.stride
            || actual.bytes < request.bytes
            || capacity < actual.bytes
        {
            return None;
        }
        // At most the capacity, which is at most MAX_ALLOCATION.
        let stride = i32::try_from(actual.stride).ok()?;
        // Serialize publication against Closing. An admitted policy may have
        // blocked while replacement/destruction began; it must not publish late.
        let phase = lock(&generation.phase);
        if *phase != Phase::Active {
            return None;
        }
        let mut s = lock(&self.state);
        if s.leases.contains_key(&(token, pointer.as_ptr() as usize)) {
            return None;
        }
        // The buffer takes the place of the grant that covered it.
        reservation.release(&mut s);
        s.bytes += capacity;
        s.leases.insert(
            (token, pointer.as_ptr() as usize),
            Lease {
                _buffer: allocation,
                bytes: capacity,
                _generation: generation.clone(),
            },
        );
        drop(s);
        drop(phase);
        Some((pointer, stride))
    }

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

/// The registry, which exists once any [`AllocatorService`] has been created.
fn registry() -> &'static Registry {
    REGISTRY
        .get()
        .expect("an allocator service exists before anything uses the registry")
}

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
}

/// A snapshot of custom allocation across the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct AllocatorUsage {
    generations: usize,
    active_generations: usize,
    retained_allocations: usize,
    retained_bytes: usize,
    calls: usize,
    reclaiming: usize,
}

impl AllocatorUsage {
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
    pub fn retained_allocations(self) -> usize {
        self.retained_allocations
    }

    /// Bytes the SDK holds, including those being released.
    pub fn retained_bytes(self) -> usize {
        self.retained_bytes
    }

    /// Policy calls running at this moment.
    pub fn in_progress_allocations(self) -> usize {
        self.calls
    }

    /// Buffers whose release is running at this moment.
    pub fn in_progress_reclamations(self) -> usize {
        self.reclaiming
    }
}

impl AllocatorService {
    /// Enable custom allocation with these process-wide limits.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`](crate::Error::InvalidInput) when the limits are
    /// out of range or differ from those of an earlier service.
    pub fn new(runtime: ProcessLifetimeRuntime, limits: AllocatorLimits) -> Result<Self> {
        let limits = limits.validate()?;
        let registry = REGISTRY.get_or_init(|| Registry::new(limits));
        if registry.limits != limits {
            return Err(crate::Error::InvalidInput(
                "Allocator limits differ from the existing process allocator service".into(),
            ));
        }
        Ok(Self { runtime })
    }

    /// What custom allocation currently holds across the process.
    pub fn usage(self) -> AllocatorUsage {
        let registry = registry();
        let state = lock(&registry.state);
        AllocatorUsage {
            generations: registry.generations.load(Ordering::Acquire),
            active_generations: state.active.len(),
            retained_allocations: state.leases.len() + state.reclaiming,
            retained_bytes: state.bytes,
            calls: state.calls,
            reclaiming: state.reclaiming,
        }
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
            .map(|inner| AllocatedReceiver { inner })
    }

    /// Take a packet receiver so that its allocators can be set.
    ///
    /// # Errors
    ///
    /// As for [`attach`](Self::attach).
    pub fn attach_packets(
        self,
        receiver: PacketReceiver,
    ) -> std::result::Result<AllocatedPacketReceiver, Rejected<PacketReceiver>> {
        match self.allocation(receiver.receiver) {
            Ok(inner) => Ok(AllocatedPacketReceiver { inner }),
            Err(rejected) => Err(rejected.map(PacketReceiver::from_receiver)),
        }
    }

    /// Take `receiver`, in any mode, with no allocator of its own set yet.
    fn allocation<M: ReceiverMode>(
        self,
        receiver: Receiver<M>,
    ) -> std::result::Result<Allocation<M>, Rejected<Receiver<M>>> {
        match self.runtime.ndi().table::<ReceiveAllocators>() {
            Ok(table) => Ok(Allocation {
                receiver: Some(receiver),
                table,
                video_token: None,
                audio_token: None,
                _service: self,
            }),
            Err(error) => Err(Rejected::new(error, receiver)),
        }
    }
}

/// A [`PacketReceiver`] whose frames can be received into application memory.
///
/// [`into_packet_receiver`](Self::into_packet_receiver) gives the packet
/// receiver back. [`receiver`](Self::receiver) reaches the receiver's
/// controls, as [`PacketReceiver::receiver`] does, and
/// [`controls`](Self::controls) reaches exclusive stream controls and KVM.
/// There is no `receiver_mut`: the allocators are installed on this
/// receiver's native instance, so it is never lent out mutably, where it
/// could be swapped for another packet receiver's.
///
/// # Threading
///
/// It is [`Send`] and [`Sync`], as the packet receiver and an
/// [`AllocatedReceiver`] are: capturing, setting and resetting allocators,
/// and dropping it can each happen on any thread. Capturing and changing an
/// allocator take it mutably; a shared reference reaches only the receiver's
/// controls.
pub struct AllocatedPacketReceiver {
    // Its Send and Sync are those of the allocation, which an
    // AllocatedReceiver shares. The SDK calls the policies on threads of its
    // own, possibly several at once, and lets the allocators be changed at
    // any time, with each allocation freed exactly once through the free
    // function it was made with (Advanced SDK manual, 25.4.2.1). Neither a
    // policy nor its registration is tied to the thread that set it: the
    // opaque value is a token into the process-wide registry, which every
    // callback locks.
    inner: Allocation<PacketAttached>,
}

impl AllocatedPacketReceiver {
    /// Wait up to `timeout` for the next frame or event; see
    /// [`PacketReceiver::capture`].
    ///
    /// # Errors
    ///
    /// As for [`PacketReceiver::capture`].
    pub fn capture(&mut self, timeout: std::time::Duration) -> Result<Option<PacketEvent>> {
        PacketReceiver::capture_from(self.inner.receiver(), timeout)
    }

    /// The receiver, for its controls; see [`PacketReceiver::receiver`].
    pub fn receiver(&self) -> &Receiver<PacketAttached> {
        self.inner.receiver()
    }

    /// Borrow exclusive stream controls or start KVM without permitting
    /// receiver replacement. This allocates nothing and makes no SDK call.
    /// A KVM session captures packets through its own `capture` method.
    pub fn controls(&mut self) -> ReceiverControls<'_, PacketAttached> {
        ReceiverControls::new(self.inner.receiver_mut())
    }

    /// Receive video into memory from `policy`; see
    /// [`AllocatedReceiver::set_video_allocator`].
    ///
    /// # Errors
    ///
    /// As for [`AllocatedReceiver::set_video_allocator`].
    pub fn set_video_allocator<P: ReceiveAllocator>(
        &mut self,
        policy: P,
    ) -> std::result::Result<(), Rejected<P>> {
        self.inner.set_allocator(Stream::Video, policy)
    }

    /// Receive audio into memory from `policy`; see
    /// [`AllocatedReceiver::set_audio_allocator`].
    ///
    /// # Errors
    ///
    /// As for [`AllocatedReceiver::set_audio_allocator`].
    pub fn set_audio_allocator<P: ReceiveAllocator>(
        &mut self,
        policy: P,
    ) -> std::result::Result<(), Rejected<P>> {
        self.inner.set_allocator(Stream::Audio, policy)
    }

    /// Return to the SDK's own video memory.
    pub fn reset_video_allocator(&mut self) {
        self.inner.install(Stream::Video, None);
    }

    /// Return to the SDK's own audio memory.
    pub fn reset_audio_allocator(&mut self) {
        self.inner.install(Stream::Audio, None);
    }

    /// Return to the SDK's own memory and give the packet receiver back.
    pub fn into_packet_receiver(self) -> PacketReceiver {
        PacketReceiver::from_receiver(self.inner.into_receiver())
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
/// Setting an allocator replaces the previous one for frames allocated from
/// then on. Frames allocated earlier stay in the memory the earlier policy
/// supplied, and that policy is kept until the SDK has freed the last of
/// them. A policy call that is still running when its policy is replaced is
/// declined.
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
/// # let source = Source::new("");
/// let receiver = Receiver::new(&ndi, &ReceiverOptions::new(source))?;
/// let service = AllocatorService::new(ndi.process_lifetime(), AllocatorLimits::default())?;
/// let mut receiver = service.attach(receiver)?;
///
/// // Pad every video line to a multiple of 256 bytes.
/// receiver.set_video_allocator(|request: AllocationRequest| {
///     let stride = request.stride_bytes().next_multiple_of(256);
///     AllocationBuffer::zeroed(request.with_stride_bytes(stride)?)
/// })?;
/// if let Some(frame) = receiver.receiver().video().capture(Duration::from_secs(1))? {
///     println!("line stride: {} bytes", frame.line_stride_bytes());
/// }
/// # Ok(())
/// # }
/// ```
pub struct AllocatedReceiver {
    inner: Allocation<RawCapture>,
}

impl AllocatedReceiver {
    /// The receiver, for capturing and control.
    pub fn receiver(&self) -> &Receiver {
        self.inner.receiver()
    }

    /// Borrow exclusive stream controls or start KVM without permitting
    /// receiver replacement. This allocates nothing and makes no SDK call.
    /// Capture through the KVM session's lent receiver while inputs are held.
    pub fn controls(&mut self) -> ReceiverControls<'_, RawCapture> {
        ReceiverControls::new(self.inner.receiver_mut())
    }

    /// Receive video into memory from `policy`.
    ///
    /// # Errors
    ///
    /// Returns the policy with [`Error::InvalidInput`] when the process already
    /// has [`AllocatorLimits::max_generations`] policies. The current
    /// allocator then stays in place.
    pub fn set_video_allocator<P: ReceiveAllocator>(
        &mut self,
        policy: P,
    ) -> std::result::Result<(), Rejected<P>> {
        self.inner.set_allocator(Stream::Video, policy)
    }

    /// Receive audio into memory from `policy`.
    ///
    /// # Errors
    ///
    /// As for [`set_video_allocator`](Self::set_video_allocator).
    pub fn set_audio_allocator<P: ReceiveAllocator>(
        &mut self,
        policy: P,
    ) -> std::result::Result<(), Rejected<P>> {
        self.inner.set_allocator(Stream::Audio, policy)
    }

    /// Return to the SDK's own video memory.
    pub fn reset_video_allocator(&mut self) {
        self.inner.install(Stream::Video, None);
    }

    /// Return to the SDK's own audio memory.
    pub fn reset_audio_allocator(&mut self) {
        self.inner.install(Stream::Audio, None);
    }

    /// Return to the SDK's own memory and give the receiver back.
    pub fn into_receiver(self) -> Receiver {
        self.inner.into_receiver()
    }
}

/// A receiver in mode `M` and the allocators set on it: what
/// [`AllocatedReceiver`] and [`AllocatedPacketReceiver`] share.
///
/// Dropping it, or taking the receiver back, returns the receiver to the
/// SDK's own memory first. The receiver itself may live on afterwards, in a
/// [`RecvAdvertiser`](crate::discovery::RecvAdvertiser) registration: the
/// allocations it still holds are then freed through the token-keyed registry,
/// which outlives every receiver.
struct Allocation<M: ReceiverMode> {
    // `None` only once `into_receiver` has taken it.
    receiver: Option<Receiver<M>>,
    table: &'static ReceiveAllocators,
    video_token: Option<usize>,
    audio_token: Option<usize>,
    // The promise that the runtime outlives every callback registered here.
    _service: AllocatorService,
}

impl<M: ReceiverMode> Allocation<M> {
    fn receiver(&self) -> &Receiver<M> {
        self.receiver.as_ref().expect("receiver owned")
    }

    fn receiver_mut(&mut self) -> &mut Receiver<M> {
        self.receiver.as_mut().expect("receiver owned")
    }

    fn set_allocator<P: ReceiveAllocator>(
        &mut self,
        stream: Stream,
        policy: P,
    ) -> std::result::Result<(), Rejected<P>> {
        let token = registry().register(policy)?;
        self.install(stream, Some(token));
        Ok(())
    }

    /// Register generation `token` (or the SDK's own allocator) for `stream`
    /// and retire the generation it replaces.
    fn install(&mut self, stream: Stream, token: Option<usize>) {
        let registry = registry();
        let slot = match stream {
            Stream::Video => &mut self.video_token,
            Stream::Audio => &mut self.audio_token,
        };
        let earlier = std::mem::replace(slot, token);
        if let Some(earlier) = earlier {
            // From here on a policy call that is still running cannot publish.
            registry.closing(earlier);
        }
        let opaque = token.map_or(std::ptr::null_mut(), |token| token as *mut c_void);
        let instance = self.receiver().core.instance;
        let table = self.table;
        // SAFETY: the receiver is live. The callbacks are functions of this
        // program, which stays loaded, and the opaque value is a token they
        // look up, never a pointer they follow.
        native!(ReceiveAllocators, table => unsafe {
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
        if let Some(earlier) = earlier {
            registry.retire(earlier);
        }
    }

    /// Return to the SDK's own memory and give the receiver back.
    fn into_receiver(mut self) -> Receiver<M> {
        self.reset();
        self.receiver.take().expect("receiver owned")
    }

    /// Return to the SDK's own memory for both streams.
    fn reset(&mut self) {
        self.install(Stream::Video, None);
        self.install(Stream::Audio, None);
    }
}

impl<M: ReceiverMode> Drop for Allocation<M> {
    fn drop(&mut self) {
        if self.receiver.is_some() {
            self.reset();
        }
    }
}

/// A compressed frame's size, which the SDK passes in the stride union.
fn compressed_size(size: i32) -> Result<usize> {
    usize::try_from(size)
        .ok()
        .filter(|size| (1..=MAX_ALLOCATION).contains(size))
        .ok_or_else(|| invalid_frame("Invalid compressed allocation size"))
}

/// A dimension of a compressed frame the SDK is about to receive.
fn compressed_dimension(value: i32) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| invalid_frame(format!("Negative compressed allocation dimension {value}")))
}

/// The request for the video frame the SDK is about to receive.
fn video_request(frame: &NDIlib_video_frame_v2_t) -> Result<AllocationRequest> {
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
        });
    }
    let pixel = PixelFormat::try_from(fourcc)
        .map_err(|_| invalid_frame("Unknown allocator video format"))?;
    let layout = ValidatedVideoLayout::from_sdk(pixel, frame.xres, frame.yres, 0)?;
    Ok(AllocationRequest {
        bytes: layout.data_len_bytes,
        stride: layout.line_stride_bytes,
        format: AllocationFormat::Video(pixel),
        width: layout.width,
        height: layout.height,
        capacity_limit: MAX_ALLOCATION,
    })
}

/// The request for the audio frame the SDK is about to receive.
fn audio_request(frame: &NDIlib_audio_frame_v3_t) -> Result<AllocationRequest> {
    if frame.FourCC != NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP {
        if AudioCodec::from_outer(frame.FourCC).is_none() {
            return Err(invalid_frame("Unknown allocator audio format"));
        }
        // SAFETY: for a compressed frame the union holds the data size.
        let size = compressed_size(unsafe { frame.__bindgen_anon_1.data_size_in_bytes })?;
        return Ok(AllocationRequest {
            bytes: size,
            stride: size,
            format: AllocationFormat::CompressedAudio(FourCc::from_raw(frame.FourCC)),
            width: compressed_dimension(frame.no_samples)?,
            height: compressed_dimension(frame.no_channels)?,
            capacity_limit: MAX_ALLOCATION,
        });
    }
    let (Some(samples), Some(channels), Some(_)) = (
        sdk_quantity(frame.no_samples),
        sdk_quantity(frame.no_channels),
        sdk_quantity(frame.sample_rate),
    ) else {
        return Err(invalid_frame("Invalid audio allocation dimensions"));
    };
    let stride = (samples as usize)
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| invalid_frame("Audio allocator stride overflow"))?;
    let bytes = stride
        .checked_mul(channels as usize)
        .filter(|n| *n <= MAX_ALLOCATION)
        .ok_or_else(|| invalid_frame("Audio allocator extent overflow"))?;
    Ok(AllocationRequest {
        bytes,
        stride,
        format: AllocationFormat::AudioPlanarF32,
        width: samples,
        height: channels,
        capacity_limit: MAX_ALLOCATION,
    })
}
/// The SDK's request for video memory. `opaque` is the generation token.
unsafe extern "C" fn video_allocate(
    opaque: *mut c_void,
    frame: *mut NDIlib_video_frame_v2_t,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the SDK passes null or a descriptor it owns for this call.
        let Some(frame) = (unsafe { frame.as_mut() }) else {
            return false;
        };
        let request = video_request(frame);
        frame.p_data = std::ptr::null_mut();
        frame.__bindgen_anon_1.line_stride_in_bytes = 0;
        let Some((pointer, stride)) = request
            .ok()
            .and_then(|request| REGISTRY.get()?.allocate(opaque as usize, request))
        else {
            return false;
        };
        frame.p_data = pointer.as_ptr();
        frame.__bindgen_anon_1.line_stride_in_bytes = stride;
        true
    }))
    .unwrap_or(false)
}

/// The SDK's request for audio memory. `opaque` is the generation token.
unsafe extern "C" fn audio_allocate(
    opaque: *mut c_void,
    frame: *mut NDIlib_audio_frame_v3_t,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the SDK passes null or a descriptor it owns for this call.
        let Some(frame) = (unsafe { frame.as_mut() }) else {
            return false;
        };
        let request = audio_request(frame);
        frame.p_data = std::ptr::null_mut();
        frame.__bindgen_anon_1.channel_stride_in_bytes = 0;
        let Some((pointer, stride)) = request
            .ok()
            .and_then(|request| REGISTRY.get()?.allocate(opaque as usize, request))
        else {
            return false;
        };
        frame.p_data = pointer.as_ptr();
        frame.__bindgen_anon_1.channel_stride_in_bytes = stride;
        true
    }))
    .unwrap_or(false)
}

/// Release the allocation at `data` that generation `opaque` made.
fn free(opaque: *mut c_void, data: *mut u8) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        REGISTRY
            .get()
            .is_some_and(|registry| registry.free(opaque as usize, data))
    }))
    .unwrap_or(false)
}

/// The SDK's release of video memory. `opaque` is the generation token.
unsafe extern "C" fn video_free(
    opaque: *mut c_void,
    frame: *const NDIlib_video_frame_v2_t,
) -> bool {
    // SAFETY: the SDK passes null or a descriptor that is valid for this call.
    unsafe { frame.as_ref() }.is_some_and(|frame| free(opaque, frame.p_data))
}

/// The SDK's release of audio memory. `opaque` is the generation token.
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
    use crate::StreamTier;
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
        assert!(matches!(video_request(&video), Err(Error::InvalidFrame(_))));
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
        }
    }

    fn request() -> AllocationRequest {
        request_of(64)
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
            assert!(reason(zero).contains("positive"));
        }
        assert!(reason(defaults.max_allocation_bytes(MAX_ALLOCATION + 1)).contains("256 MiB"));
        assert!(reason(defaults.max_allocations(15)).contains("max_allocations"));
        // Each of these promises more concurrent calls than the byte budget
        // can grant a largest allocation to.
        assert!(reason(defaults.max_concurrent_calls(17)).contains("max_bytes"));
        assert!(reason(defaults.max_allocation_bytes(64 * MIB + 1)).contains("max_bytes"));
        assert!(reason(defaults.max_bytes(1024 * MIB - 1)).contains("max_bytes"));
        assert!(reason(
            defaults
                .max_concurrent_calls(32)
                .max_allocation_bytes(MAX_ALLOCATION)
        )
        .contains("max_bytes"));
        assert!(reason(
            defaults
                .max_bytes(usize::MAX)
                .max_allocations(usize::MAX)
                .max_concurrent_calls(usize::MAX)
        )
        .contains("max_bytes"));

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
                None => assert!(reason(limits.max_bytes(usize::MAX)).contains("max_bytes")),
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
        let registry = Registry::new(limits);
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
        assert_eq!(admitted.len(), calls);
        assert_eq!(*lock(&granted), vec![limits.max_allocation_bytes; calls]);

        let state = lock(&registry.state);
        assert_eq!((state.calls, state.reserved_bytes), (0, 0));
        assert_eq!((state.leases.len(), state.bytes), (calls, calls * 64));
        drop(state);
        for address in admitted {
            assert!(registry.free(token, address as *mut u8));
        }
        registry.retire(token);
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
        );
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

        // Freeing returns exactly what the buffer took.
        assert!(registry.free(token, held[0].as_ptr()));
        assert!(registry.allocate(token, request()).is_some());
        assert_eq!(lock(&granted).last(), Some(&64));
    }

    #[test]
    fn a_buffer_beyond_its_grant_is_declined_and_nothing_stays_charged() {
        let registry = Registry::new(limits());
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
        let registry = Registry::new(limits());
        let first = registry.register(Tagged(1)).unwrap();
        let second = registry.register(Tagged(2)).unwrap();
        assert_eq!((first, second), (1, 2));

        // The generation budget is exhausted.
        let rejected = registry.register(Tagged(3)).unwrap_err();
        assert!(
            matches!(rejected.error(), Error::InvalidInput(reason) if reason.contains("generation budget"))
        );
        assert_eq!(rejected.into_inner().0, 3);
        assert_eq!(registry.generations.load(Ordering::Acquire), 2);
        assert_eq!(lock(&registry.state).active.len(), 2);

        // Room again, but no token left to name a generation with: the
        // generation that was counted for the attempt is given up.
        registry.retire(second);
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        registry.next.store(usize::MAX, Ordering::Release);
        let rejected = registry.register(Tagged(4)).unwrap_err();
        assert!(rejected.error().to_string().contains("token space"));
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
        let refused = |storage: OwnedStorage, problem: &str| {
            let address = storage.pointer();
            let rejected = AllocationBuffer::from_external(request, storage)
                .err()
                .expect("the storage does not fit the request");
            assert!(
                matches!(rejected.error(), Error::InvalidInput(reason) if reason.contains(problem))
            );
            assert_eq!(rejected.into_inner().pointer(), address);
        };
        refused(OwnedStorage::zeroed(32).unwrap(), "smaller");
        refused(OwnedStorage::zeroed(192).unwrap(), "larger");
        let rejected = AllocationBuffer::from_external(
            request,
            Misaligned(OwnedStorage::zeroed(128).unwrap()),
        )
        .err()
        .expect("the storage is misaligned");
        assert!(rejected.error().to_string().contains("aligned"));
        assert_eq!(rejected.into_inner().0.capacity(), 128);

        let buffer =
            AllocationBuffer::from_external(request, OwnedStorage::zeroed(128).unwrap()).unwrap();
        assert_eq!(buffer.capacity(), 128);
        assert!(OwnedStorage::zeroed(0).is_err());
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
        assert!(input(planar.with_stride_bytes(15)).contains("smaller"));
        assert!(input(planar.with_stride_bytes(17)).contains("even"));

        let audio = request();
        assert!(input(audio.with_stride_bytes(34)).contains("whole number"));
        let limited = AllocationRequest {
            capacity_limit: 64,
            ..audio
        };
        assert!(input(limited.with_stride_bytes(36)).contains("capacity limit"));

        let compressed = AllocationRequest {
            format: AllocationFormat::CompressedAudio(FourCc::from_raw(
                AudioCodec::Opus.outer(false),
            )),
            ..audio
        };
        assert_eq!(compressed.with_stride_bytes(32).unwrap(), compressed);
        assert!(input(compressed.with_stride_bytes(36)).contains("Compressed"));
    }

    #[test]
    fn retired_lease_keeps_generation_and_budget_until_matching_free() {
        let registry = Registry::new(limits());
        let token = registry.register(AllocationBuffer::zeroed).unwrap();
        let (pointer, _) = registry.allocate(token, request()).unwrap();
        registry.retire(token);
        assert!(registry.allocate(token, request()).is_none());
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        let next = registry.register(AllocationBuffer::zeroed).unwrap();
        assert_ne!(token, next);
        assert!(registry.register(AllocationBuffer::zeroed).is_err());
        assert!(registry.free(token, pointer.as_ptr()));
        assert!(!registry.free(token, pointer.as_ptr()));
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        registry.retire(next);
    }

    #[test]
    fn a_free_releases_only_an_allocation_of_the_generation_it_names() {
        let registry = Registry::new(limits());
        let first = registry.register(AllocationBuffer::zeroed).unwrap();
        let (pointer, _) = registry.allocate(first, request()).unwrap();
        // The receiver moves on to a second generation while the SDK still
        // holds the first one's buffer.
        registry.retire(first);
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
        registry.retire(second);
        assert_eq!(registry.generations.load(Ordering::Acquire), 0);
    }

    #[test]
    fn policy_cannot_publish_after_close() {
        let registry = Registry::new(limits());
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
            registry.retire(token);
            gate.open();
            worker.join().unwrap()
        });
        assert!(!published);
        assert_eq!(lock(&registry.state).bytes, 0);
        assert_eq!(registry.generations.load(Ordering::Acquire), 0);
    }

    #[test]
    fn budgets_cover_multiple_generations_and_panic_paths() {
        let registry = Registry::new(limits());
        let first = registry.register(AllocationBuffer::zeroed).unwrap();
        let second = registry.register(AllocationBuffer::zeroed).unwrap();
        let (a, _) = registry.allocate(first, request()).unwrap();
        let (b, _) = registry.allocate(second, request()).unwrap();
        assert!(registry.allocate(first, request()).is_none());
        registry.retire(first);
        registry.retire(second);
        assert!(registry.free(first, a.as_ptr()));
        assert!(registry.free(second, b.as_ptr()));
        let panic = registry
            .register(|_| -> Result<AllocationBuffer> { panic!("policy") })
            .unwrap();
        assert!(registry.allocate(panic, request()).is_none());
        assert_eq!(lock(&registry.state).calls, 0);
        assert_eq!(lock(&registry.state).reserved_bytes, 0);
        registry.retire(panic);
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
        );
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
        registry.retire(token);
    }
}
