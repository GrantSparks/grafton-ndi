//! Receive allocation policies. Native callbacks use non-dereferenced generation tokens;
//! allocations retain their generation independently until the matching native free.
use super::ffi;
use crate::runtime::ProcessLifetimeRuntime;
use crate::{ndi_lib::*, LineStrideOrSize, PixelFormat, Receiver, Result};
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

const MAX_ALLOCATION: usize = 256 * 1024 * 1024;

/// Process-wide admitted-buffer limits include old generations and buffers not
/// yet reclaimed. Arbitrary working allocations made inside a user policy are
/// outside this accounting; wrapper allocations honor the request capacity limit.
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
        Self {
            max_generations: 256,
            max_allocations: 1024,
            max_bytes: 1024 * 1024 * 1024,
            max_concurrent_calls: 32,
            max_allocation_bytes: MAX_ALLOCATION,
        }
    }
}
impl AllocatorLimits {
    pub fn max_generations(mut self, value: usize) -> Self {
        self.max_generations = value;
        self
    }
    pub fn max_allocations(mut self, value: usize) -> Self {
        self.max_allocations = value;
        self
    }
    pub fn max_bytes(mut self, value: usize) -> Self {
        self.max_bytes = value;
        self
    }
    pub fn max_concurrent_calls(mut self, value: usize) -> Self {
        self.max_concurrent_calls = value;
        self
    }
    pub fn max_allocation_bytes(mut self, value: usize) -> Self {
        self.max_allocation_bytes = value;
        self
    }
    fn validate(self) -> Result<Self> {
        if self.max_generations == 0
            || self.max_allocations == 0
            || self.max_concurrent_calls == 0
            || self.max_allocation_bytes == 0
            || self.max_allocation_bytes > MAX_ALLOCATION
            || self.max_bytes < self.max_allocation_bytes
        {
            return Err(ffi::invalid("Invalid allocator limits"));
        }
        Ok(self)
    }
}

/// A validated writable allocation request; no native frame pointer escapes.
#[derive(Clone, Copy, Debug)]
pub struct AllocationRequest {
    bytes: usize,
    stride: i32,
    format: AllocationFormat,
    width: i32,
    height: i32,
    capacity_limit: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AllocationFormat {
    Video(PixelFormat),
    AudioPlanarF32,
    CompressedVideo(u32),
    CompressedAudio(u32),
}
impl AllocationRequest {
    pub fn capacity_limit(self) -> usize {
        self.capacity_limit
    }
    pub fn minimum_bytes(self) -> usize {
        self.bytes
    }
    pub fn stride(self) -> i32 {
        self.stride
    }
    pub fn format(self) -> AllocationFormat {
        self.format
    }
    pub fn dimensions(self) -> (i32, i32) {
        (self.width, self.height)
    }
    /// Creates a request with a larger row/channel stride and checked storage extent.
    pub fn with_stride(self, stride: i32) -> Result<Self> {
        if stride < self.stride {
            return Err(ffi::invalid(
                "Allocator stride is smaller than the requested stride",
            ));
        }
        let bytes = match self.format {
            AllocationFormat::Video(pixel) => {
                crate::frames::ValidatedVideoLayout::new_uncompressed(
                    pixel,
                    self.width,
                    self.height,
                    Some(stride),
                )?
                .data_len_bytes
            }
            AllocationFormat::AudioPlanarF32 => {
                if stride % 4 != 0 {
                    return Err(ffi::invalid(
                        "Audio allocator stride must be aligned to f32",
                    ));
                }
                (stride as usize)
                    .checked_mul(self.height as usize)
                    .ok_or_else(|| ffi::invalid("Audio allocator extent overflow"))?
            }
            _ if stride == self.stride => self.bytes,
            _ => {
                return Err(ffi::invalid(
                    "Compressed allocations have a byte size, not a configurable stride",
                ))
            }
        };
        if bytes > self.capacity_limit {
            return Err(ffi::invalid("Allocator extent exceeds the buffer limit"));
        }
        Ok(Self {
            bytes,
            stride,
            ..self
        })
    }
}

/// Storage supplied by an integration with a native memory pool.
///
/// # Safety
/// The pointer must remain uniquely owned, writable, CPU accessible, and aligned to
/// at least 64 bytes for `capacity` initialized bytes until this object is dropped. NDI may write
/// from arbitrary native threads. The destructor must permit that thread and must
/// not release or remap the allocation early. No other access is permitted while
/// the object is owned by NDI. The capacity and address must remain constant.
/// The destructor must reclaim the storage without panicking.
pub unsafe trait ExternalAllocation: Send + 'static {
    fn pointer(&self) -> NonNull<u8>;
    fn capacity(&self) -> usize;
}
struct OwnedStorage {
    pointer: NonNull<u8>,
    layout: Layout,
}
unsafe impl Send for OwnedStorage {}
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
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
    }
}

/// An owned allocation. All bytes are initialized when created with `zeroed`.
pub struct AllocationBuffer {
    storage: Box<dyn ExternalAllocation>,
    request: AllocationRequest,
}
impl AllocationBuffer {
    pub fn zeroed(request: AllocationRequest) -> Result<Self> {
        let layout = Layout::from_size_align(request.bytes, 64)
            .map_err(|_| ffi::invalid("Invalid allocator layout"))?;
        let pointer = NonNull::new(unsafe { alloc_zeroed(layout) })
            .ok_or_else(|| ffi::invalid("Allocation failed"))?;
        Ok(Self {
            storage: Box::new(OwnedStorage { pointer, layout }),
            request,
        })
    }
    /// The unsafe trait implementation carries the native storage obligations.
    pub fn from_external(
        request: AllocationRequest,
        storage: impl ExternalAllocation,
    ) -> Result<Self> {
        if storage.capacity() < request.bytes
            || storage.capacity() > request.capacity_limit
            || !(storage.pointer().as_ptr() as usize).is_multiple_of(64)
        {
            return Err(ffi::invalid(
                "External allocation capacity or alignment is invalid",
            ));
        }
        Ok(Self {
            storage: Box::new(storage),
            request,
        })
    }
    pub fn capacity(&self) -> usize {
        self.storage.capacity()
    }
}

/// Called on SDK threads, possibly concurrently and reentrantly. Implementations
/// should avoid blocking. Panics are contained and become allocation failures.
pub trait ReceiveAllocator: Send + Sync + 'static {
    fn allocate(&self, request: AllocationRequest) -> Result<AllocationBuffer>;
}
impl<F: Fn(AllocationRequest) -> Result<AllocationBuffer> + Send + Sync + 'static> ReceiveAllocator
    for F
{
    fn allocate(&self, request: AllocationRequest) -> Result<AllocationBuffer> {
        self(request)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Active,
    Closing,
    Retired,
}
struct Generation {
    phase: Mutex<Phase>,
    policy: ManuallyDrop<Arc<dyn ReceiveAllocator>>,
    count: Arc<AtomicUsize>,
}
impl Drop for Generation {
    fn drop(&mut self) {
        // Keep the generation budget charged while user policy destruction runs.
        // If it panics, retain that charge: reclamation may be incomplete.
        let policy = unsafe { ManuallyDrop::take(&mut self.policy) };
        if catch_unwind(AssertUnwindSafe(|| drop(policy))).is_ok() {
            self.count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
struct Lease {
    _buffer: AllocationBuffer,
    bytes: usize,
    _generation: Arc<Generation>,
}
#[derive(Default)]
struct State {
    active: HashMap<usize, Arc<Generation>>,
    leases: HashMap<(usize, usize), Lease>,
    bytes: usize,
    reserved_bytes: usize,
    calls: usize,
    reclaiming: usize,
}
struct Registry {
    limits: AllocatorLimits,
    state: Mutex<State>,
    generations: Arc<AtomicUsize>,
    next: AtomicUsize,
}
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
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
    fn register(&self, policy: Arc<dyn ReceiveAllocator>) -> Result<usize> {
        // Rust 1.99 deprecated fetch_update in favor of try_update, which is
        // unavailable on our Rust 1.88 MSRV. Limit the allowance to these calls.
        #[allow(deprecated)]
        let admission = self
            .generations
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.limits.max_generations).then_some(n + 1)
            });
        admission.map_err(|_| ffi::invalid("Process allocator generation budget exhausted"))?;
        #[allow(deprecated)]
        let next_token = self
            .next
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1));
        let token = match next_token {
            Ok(token) => token,
            Err(_) => {
                self.generations.fetch_sub(1, Ordering::AcqRel);
                return Err(ffi::invalid("Allocator token space exhausted"));
            }
        };
        lock(&self.state).active.insert(
            token,
            Arc::new(Generation {
                phase: Mutex::new(Phase::Active),
                policy: ManuallyDrop::new(policy),
                count: self.generations.clone(),
            }),
        );
        Ok(token)
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
        request.capacity_limit = self.limits.max_allocation_bytes;
        let generation = {
            let mut s = lock(&self.state);
            let generation = s.active.get(&token)?.clone();
            // Reserve the maximum allowed return before invoking user code. This
            // bounds simultaneous policy returns and includes their padding.
            if request.bytes > self.limits.max_allocation_bytes
                || s.calls >= self.limits.max_concurrent_calls
                || s.leases.len() + s.calls + s.reclaiming >= self.limits.max_allocations
                || s.bytes
                    .checked_add(s.reserved_bytes)?
                    .checked_add(self.limits.max_allocation_bytes)?
                    > self.limits.max_bytes
            {
                return None;
            }
            s.calls += 1;
            s.reserved_bytes += self.limits.max_allocation_bytes;
            generation
        };
        struct Reservation<'a>(&'a Registry);
        impl Drop for Reservation<'_> {
            fn drop(&mut self) {
                let mut s = lock(&self.0.state);
                s.calls -= 1;
                s.reserved_bytes -= self.0.limits.max_allocation_bytes;
            }
        }
        let reservation = Reservation(self);
        if *lock(&generation.phase) != Phase::Active {
            return None;
        }
        let allocation = catch_unwind(AssertUnwindSafe(|| generation.policy.allocate(request)))
            .ok()?
            .ok()?;
        let pointer = allocation.storage.pointer();
        let capacity = allocation.capacity();
        let actual = allocation.request;
        if capacity > self.limits.max_allocation_bytes
            || actual.format != request.format
            || actual.width != request.width
            || actual.height != request.height
            || actual.stride < request.stride
            || actual.bytes < request.bytes
            || capacity < actual.bytes
        {
            return None;
        }
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
        s.bytes += capacity;
        let stride = actual.stride;
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
        drop(reservation);
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

/// Explicit process-lifetime SDK opt-in and shared allocator limits. The first
/// service fixes the limits for the process; later services must agree.
/// An embedding host must also keep the Rust module containing these callbacks
/// loaded for the process lifetime; normal Rust application linkage already does.
#[derive(Clone, Copy)]
pub struct AllocatorService {
    runtime: ProcessLifetimeRuntime,
}

/// A process-wide allocator budget snapshot. A panicking policy destructor can
/// leave a conservative permanent generation charge; external storage must not panic.
#[derive(Clone, Copy, Debug)]
pub struct AllocatorUsage {
    generations: usize,
    active_generations: usize,
    retained_allocations: usize,
    retained_bytes: usize,
    calls: usize,
    reclaiming: usize,
}
impl AllocatorUsage {
    pub fn generations(self) -> usize {
        self.generations
    }
    pub fn active_generations(self) -> usize {
        self.active_generations
    }
    pub fn retained_allocations(self) -> usize {
        self.retained_allocations
    }
    pub fn retained_bytes(self) -> usize {
        self.retained_bytes
    }
    pub fn in_progress_allocations(self) -> usize {
        self.calls
    }
    pub fn in_progress_reclamations(self) -> usize {
        self.reclaiming
    }
}
impl AllocatorService {
    pub fn new(runtime: ProcessLifetimeRuntime, limits: AllocatorLimits) -> Result<Self> {
        let limits = limits.validate()?;
        let registry = REGISTRY.get_or_init(|| Registry::new(limits));
        if registry.limits != limits {
            return Err(ffi::invalid(
                "Allocator limits differ from the existing process allocator service",
            ));
        }
        Ok(Self { runtime })
    }
    /// Inspect retained ownership and budget pressure, including old generations.
    pub fn usage(self) -> AllocatorUsage {
        let registry = REGISTRY.get().expect("allocator service initialized");
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
    /// Attach policies to a receiver whose results explicitly distinguish codec
    /// packets from decoded fallback. Raw decoded capture remains inaccessible.
    pub fn attach_packets(
        self,
        receiver: super::PacketReceiver,
    ) -> Result<AllocatedPacketReceiver> {
        Ok(AllocatedPacketReceiver {
            inner: self.attach(receiver.receiver)?,
        })
    }
    /// Takes exclusive control of allocator registration. Capture methods remain
    /// available through a shared borrow, which cannot outlive this owner.
    pub fn attach(self, receiver: Receiver) -> Result<AllocatedReceiver> {
        type VideoSet = unsafe extern "C" fn(
            NDIlib_recv_instance_t,
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, *mut NDIlib_video_frame_v2_t) -> bool>,
            Option<unsafe extern "C" fn(*mut c_void, *const NDIlib_video_frame_v2_t) -> bool>,
        );
        type AudioSet = unsafe extern "C" fn(
            NDIlib_recv_instance_t,
            *mut c_void,
            Option<unsafe extern "C" fn(*mut c_void, *mut NDIlib_audio_frame_v3_t) -> bool>,
            Option<unsafe extern "C" fn(*mut c_void, *const NDIlib_audio_frame_v3_t) -> bool>,
        );
        let video: VideoSet =
            unsafe { ffi::required(self.runtime.ndi(), b"NDIlib_recv_set_video_allocator\0")? };
        let audio: AudioSet =
            unsafe { ffi::required(self.runtime.ndi(), b"NDIlib_recv_set_audio_allocator\0")? };
        Ok(AllocatedReceiver {
            receiver: Some(receiver),
            service: self,
            video,
            audio,
            video_token: None,
            audio_token: None,
        })
    }
}

/// Packet capture with custom allocators and the same generation-safe reset rules.
pub struct AllocatedPacketReceiver {
    inner: AllocatedReceiver,
}
impl AllocatedPacketReceiver {
    pub fn capture(&mut self, timeout: std::time::Duration) -> Result<super::PacketEvent> {
        super::PacketReceiver::capture_from(self.inner.receiver(), timeout)
    }
    pub fn set_video_allocator(&mut self, policy: impl ReceiveAllocator) -> Result<()> {
        self.inner.set_video_allocator(policy)
    }
    pub fn set_audio_allocator(&mut self, policy: impl ReceiveAllocator) -> Result<()> {
        self.inner.set_audio_allocator(policy)
    }
    pub fn reset_video_allocator(&mut self) {
        self.inner.reset_video_allocator();
    }
    pub fn reset_audio_allocator(&mut self) {
        self.inner.reset_audio_allocator();
    }
    pub fn request_keyframe(&self) -> Result<()> {
        self.inner.receiver().request_keyframe()
    }
    pub fn set_video_quality(&mut self, quality: super::VideoQuality) -> Result<bool> {
        self.inner
            .receiver
            .as_mut()
            .expect("receiver owned")
            .set_video_quality(quality)
    }
    pub fn set_streams(&mut self, streams: super::StreamSelection) -> Result<super::StreamChange> {
        self.inner
            .receiver
            .as_mut()
            .expect("receiver owned")
            .set_streams(streams)
    }
}
type VideoSet = unsafe extern "C" fn(
    NDIlib_recv_instance_t,
    *mut c_void,
    Option<unsafe extern "C" fn(*mut c_void, *mut NDIlib_video_frame_v2_t) -> bool>,
    Option<unsafe extern "C" fn(*mut c_void, *const NDIlib_video_frame_v2_t) -> bool>,
);
type AudioSet = unsafe extern "C" fn(
    NDIlib_recv_instance_t,
    *mut c_void,
    Option<unsafe extern "C" fn(*mut c_void, *mut NDIlib_audio_frame_v3_t) -> bool>,
    Option<unsafe extern "C" fn(*mut c_void, *const NDIlib_audio_frame_v3_t) -> bool>,
);

/// An allocator owner. Replacement and reset leave old allocations alive until
/// the SDK releases them; a concurrent pending allocation can be refused.
pub struct AllocatedReceiver {
    receiver: Option<Receiver>,
    service: AllocatorService,
    video: VideoSet,
    audio: AudioSet,
    video_token: Option<usize>,
    audio_token: Option<usize>,
}
impl AllocatedReceiver {
    pub fn receiver(&self) -> &Receiver {
        self.receiver.as_ref().expect("receiver owned")
    }
    pub fn set_video_allocator(&mut self, policy: impl ReceiveAllocator) -> Result<()> {
        self.replace(true, Some(Arc::new(policy)))
    }
    pub fn set_audio_allocator(&mut self, policy: impl ReceiveAllocator) -> Result<()> {
        self.replace(false, Some(Arc::new(policy)))
    }
    pub fn reset_video_allocator(&mut self) {
        self.replace(true, None)
            .expect("reset needs no registration");
    }
    pub fn reset_audio_allocator(&mut self) {
        self.replace(false, None)
            .expect("reset needs no registration");
    }
    fn replace(&mut self, video: bool, policy: Option<Arc<dyn ReceiveAllocator>>) -> Result<()> {
        let registry = REGISTRY.get().expect("allocator service initialized");
        let new_token = policy.map(|policy| registry.register(policy)).transpose()?;
        let old_token = if video {
            self.video_token
        } else {
            self.audio_token
        };
        if let Some(old) = old_token {
            registry.closing(old);
        }
        let opaque = new_token.map_or(std::ptr::null_mut(), |token| token as *mut c_void);
        let instance = self.receiver().core.instance;
        unsafe {
            if video {
                (self.video)(
                    instance,
                    opaque,
                    new_token.map(|_| video_allocate as _),
                    new_token.map(|_| video_free as _),
                );
            } else {
                (self.audio)(
                    instance,
                    opaque,
                    new_token.map(|_| audio_allocate as _),
                    new_token.map(|_| audio_free as _),
                );
            }
        }
        if video {
            self.video_token = new_token;
        } else {
            self.audio_token = new_token;
        }
        if let Some(old) = old_token {
            registry.retire(old);
        }
        Ok(())
    }
    pub fn into_receiver(mut self) -> Receiver {
        self.reset_video_allocator();
        self.reset_audio_allocator();
        self.receiver.take().expect("receiver owned")
    }
}
impl Drop for AllocatedReceiver {
    fn drop(&mut self) {
        if self.receiver.is_some() {
            self.reset_video_allocator();
            self.reset_audio_allocator();
        }
        let _ = self.service;
    }
}

fn video_request(frame: &NDIlib_video_frame_v2_t) -> Result<AllocationRequest> {
    let fourcc = ffi::video_fourcc_bits(frame.FourCC);
    if super::VideoCodec::from_outer(fourcc).is_some() {
        let size = unsafe { frame.__bindgen_anon_1.data_size_in_bytes };
        if size <= 0 || size as usize > MAX_ALLOCATION {
            return Err(ffi::invalid("Invalid compressed video allocation size"));
        }
        return Ok(AllocationRequest {
            bytes: size as usize,
            stride: size,
            format: AllocationFormat::CompressedVideo(fourcc),
            width: frame.xres,
            height: frame.yres,
            capacity_limit: MAX_ALLOCATION,
        });
    }
    let pixel = PixelFormat::try_from(fourcc)
        .map_err(|_| ffi::invalid("Unknown allocator video format"))?;
    let layout =
        crate::frames::ValidatedVideoLayout::new_uncompressed(pixel, frame.xres, frame.yres, None)?;
    let LineStrideOrSize::LineStrideBytes(stride) = layout.line_stride_or_size;
    Ok(AllocationRequest {
        bytes: layout.data_len_bytes,
        stride,
        format: AllocationFormat::Video(pixel),
        width: frame.xres,
        height: frame.yres,
        capacity_limit: MAX_ALLOCATION,
    })
}
fn audio_request(frame: &NDIlib_audio_frame_v3_t) -> Result<AllocationRequest> {
    let fourcc = ffi::audio_fourcc_bits(frame.FourCC);
    if frame.FourCC != NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP {
        let known = [
            0xff,
            0xff | 0x8080_8080,
            u32::from_le_bytes(*b"Opus"),
            u32::from_le_bytes(*b"Opus") | 0x8080_8080,
        ];
        let size = unsafe { frame.__bindgen_anon_1.data_size_in_bytes };
        if !known.contains(&fourcc) || size <= 0 || size as usize > MAX_ALLOCATION {
            return Err(ffi::invalid("Invalid compressed audio allocation request"));
        }
        return Ok(AllocationRequest {
            bytes: size as usize,
            stride: size,
            format: AllocationFormat::CompressedAudio(fourcc),
            width: frame.no_samples,
            height: frame.no_channels,
            capacity_limit: MAX_ALLOCATION,
        });
    }
    if frame.no_samples <= 0 || frame.no_channels <= 0 || frame.sample_rate <= 0 {
        return Err(ffi::invalid("Invalid audio allocation dimensions"));
    }
    let stride = frame
        .no_samples
        .checked_mul(4)
        .ok_or_else(|| ffi::invalid("Audio allocator stride overflow"))?;
    let bytes = (stride as usize)
        .checked_mul(frame.no_channels as usize)
        .filter(|n| *n <= MAX_ALLOCATION)
        .ok_or_else(|| ffi::invalid("Audio allocator extent overflow"))?;
    Ok(AllocationRequest {
        bytes,
        stride,
        format: AllocationFormat::AudioPlanarF32,
        width: frame.no_samples,
        height: frame.no_channels,
        capacity_limit: MAX_ALLOCATION,
    })
}
unsafe extern "C" fn video_allocate(
    opaque: *mut c_void,
    frame: *mut NDIlib_video_frame_v2_t,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(frame) = (unsafe { frame.as_mut() }) else {
            return false;
        };
        let request = video_request(frame);
        frame.p_data = std::ptr::null_mut();
        frame.__bindgen_anon_1.line_stride_in_bytes = 0;
        let Ok(request) = request else {
            return false;
        };
        let Some((pointer, stride)) = REGISTRY
            .get()
            .and_then(|r| r.allocate(opaque as usize, request))
        else {
            return false;
        };
        frame.p_data = pointer.as_ptr();
        frame.__bindgen_anon_1.line_stride_in_bytes = stride;
        true
    }))
    .unwrap_or(false)
}
unsafe extern "C" fn audio_allocate(
    opaque: *mut c_void,
    frame: *mut NDIlib_audio_frame_v3_t,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(frame) = (unsafe { frame.as_mut() }) else {
            return false;
        };
        let request = audio_request(frame);
        frame.p_data = std::ptr::null_mut();
        frame.__bindgen_anon_1.channel_stride_in_bytes = 0;
        let Ok(request) = request else {
            return false;
        };
        let Some((pointer, stride)) = REGISTRY
            .get()
            .and_then(|r| r.allocate(opaque as usize, request))
        else {
            return false;
        };
        frame.p_data = pointer.as_ptr();
        frame.__bindgen_anon_1.channel_stride_in_bytes = stride;
        true
    }))
    .unwrap_or(false)
}
unsafe extern "C" fn video_free(
    opaque: *mut c_void,
    frame: *const NDIlib_video_frame_v2_t,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        unsafe { frame.as_ref() }.is_some_and(|frame| {
            REGISTRY
                .get()
                .is_some_and(|r| r.free(opaque as usize, frame.p_data))
        })
    }))
    .unwrap_or(false)
}
unsafe extern "C" fn audio_free(
    opaque: *mut c_void,
    frame: *const NDIlib_audio_frame_v3_t,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        unsafe { frame.as_ref() }.is_some_and(|frame| {
            REGISTRY
                .get()
                .is_some_and(|r| r.free(opaque as usize, frame.p_data))
        })
    }))
    .unwrap_or(false)
}

#[cfg(ndi_has_advanced_header)]
const _: () = {
    let _: VideoSet = crate::ndi_lib::raw::NDIlib_recv_set_video_allocator;
    let _: AudioSet = crate::ndi_lib::raw::NDIlib_recv_set_audio_allocator;
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_allocation_formats_preserve_protection_and_decoded_formats() {
        let protected_video = u32::from_le_bytes(*b"H264") | 0x8080_8080;
        let mut video = NDIlib_video_frame_v2_t {
            FourCC: ffi::video_fourcc_native(protected_video),
            xres: 16,
            yres: 16,
            ..Default::default()
        };
        video.__bindgen_anon_1.data_size_in_bytes = 64;
        assert_eq!(
            video_request(&video).unwrap().format(),
            AllocationFormat::CompressedVideo(protected_video)
        );
        video.FourCC = PixelFormat::BGRA.into();
        assert_eq!(
            video_request(&video).unwrap().format(),
            AllocationFormat::Video(PixelFormat::BGRA)
        );
        video.FourCC = ffi::video_fourcc_native(u32::MAX);
        assert!(video_request(&video).is_err());
        for codec in [0xff, u32::from_le_bytes(*b"Opus")] {
            for protection in [0, 0x8080_8080] {
                let bits = codec | protection;
                let mut audio = NDIlib_audio_frame_v3_t {
                    FourCC: ffi::audio_fourcc_native(bits),
                    ..Default::default()
                };
                audio.__bindgen_anon_1.data_size_in_bytes = 64;
                assert_eq!(
                    audio_request(&audio).unwrap().format(),
                    AllocationFormat::CompressedAudio(bits)
                );
                audio.FourCC = ffi::audio_fourcc_native(u32::MAX);
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
    }
    fn request() -> AllocationRequest {
        AllocationRequest {
            bytes: 64,
            stride: 32,
            format: AllocationFormat::AudioPlanarF32,
            width: 8,
            height: 2,
            capacity_limit: 64,
        }
    }
    #[test]
    fn retired_lease_keeps_generation_and_budget_until_matching_free() {
        let registry = Registry::new(limits());
        let token = registry
            .register(Arc::new(AllocationBuffer::zeroed))
            .unwrap();
        let (pointer, _) = registry.allocate(token, request()).unwrap();
        registry.retire(token);
        assert!(registry.allocate(token, request()).is_none());
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        let next = registry
            .register(Arc::new(AllocationBuffer::zeroed))
            .unwrap();
        assert_ne!(token, next);
        assert!(registry
            .register(Arc::new(AllocationBuffer::zeroed))
            .is_err());
        assert!(registry.free(token, pointer.as_ptr()));
        assert!(!registry.free(token, pointer.as_ptr()));
        assert_eq!(registry.generations.load(Ordering::Acquire), 1);
        registry.retire(next);
    }
    #[test]
    fn policy_cannot_publish_after_close() {
        let registry = Arc::new(Registry::new(limits()));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let resume_rx = Mutex::new(resume_rx);
        let token = registry
            .register(Arc::new(move |request| {
                entered_tx.send(()).unwrap();
                lock(&resume_rx).recv().unwrap();
                AllocationBuffer::zeroed(request)
            }))
            .unwrap();
        let worker = registry.clone();
        let thread = std::thread::spawn(move || {
            worker
                .allocate(token, request())
                .map(|(p, s)| (p.as_ptr() as usize, s))
        });
        entered_rx.recv().unwrap();
        registry.closing(token);
        registry.retire(token);
        resume_tx.send(()).unwrap();
        assert!(thread.join().unwrap().is_none());
        assert_eq!(lock(&registry.state).bytes, 0);
        assert_eq!(registry.generations.load(Ordering::Acquire), 0);
    }
    #[test]
    fn budgets_cover_multiple_generations_and_panic_paths() {
        let registry = Registry::new(limits());
        let first = registry
            .register(Arc::new(AllocationBuffer::zeroed))
            .unwrap();
        let second = registry
            .register(Arc::new(AllocationBuffer::zeroed))
            .unwrap();
        let (a, _) = registry.allocate(first, request()).unwrap();
        let (b, _) = registry.allocate(second, request()).unwrap();
        assert!(registry.allocate(first, request()).is_none());
        registry.retire(first);
        registry.retire(second);
        assert!(registry.free(first, a.as_ptr()));
        assert!(registry.free(second, b.as_ptr()));
        let panic = registry
            .register(Arc::new(|_| -> Result<AllocationBuffer> {
                panic!("policy")
            }))
            .unwrap();
        assert!(registry.allocate(panic, request()).is_none());
        assert_eq!(lock(&registry.state).calls, 0);
        assert_eq!(lock(&registry.state).reserved_bytes, 0);
        registry.retire(panic);
    }
    #[test]
    fn blocked_storage_reclamation_remains_charged() {
        struct DelayedStorage {
            storage: AllocationBuffer,
            entered: std::sync::mpsc::Sender<()>,
            resume: std::sync::mpsc::Receiver<()>,
        }
        unsafe impl ExternalAllocation for DelayedStorage {
            fn pointer(&self) -> NonNull<u8> {
                self.storage.storage.pointer()
            }
            fn capacity(&self) -> usize {
                self.storage.capacity()
            }
        }
        impl Drop for DelayedStorage {
            fn drop(&mut self) {
                self.entered.send(()).unwrap();
                self.resume.recv().unwrap();
            }
        }
        let registry = Arc::new(Registry::new(AllocatorLimits {
            max_allocations: 1,
            ..limits()
        }));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let resume_rx = Mutex::new(Some(resume_rx));
        let token = registry
            .register(Arc::new(move |request| {
                AllocationBuffer::from_external(
                    request,
                    DelayedStorage {
                        storage: AllocationBuffer::zeroed(request)?,
                        entered: entered_tx.clone(),
                        resume: lock(&resume_rx).take().unwrap(),
                    },
                )
            }))
            .unwrap();
        let (pointer, _) = registry.allocate(token, request()).unwrap();
        let pointer = pointer.as_ptr() as usize;
        let worker = registry.clone();
        let free = std::thread::spawn(move || worker.free(token, pointer as *mut u8));
        entered_rx.recv().unwrap();
        assert_eq!(lock(&registry.state).bytes, 64);
        assert_eq!(lock(&registry.state).reclaiming, 1);
        assert!(registry.allocate(token, request()).is_none());
        resume_tx.send(()).unwrap();
        assert!(free.join().unwrap());
        assert_eq!(lock(&registry.state).bytes, 0);
        registry.retire(token);
    }
}
