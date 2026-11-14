//! Video from a PipeWire stream (F7, milestone 5): how a Wayland desktop
//! hands out its screen, through the ScreenCast portal ([`crate::wayland`]).
//!
//! libpipewire is loaded at run time (`libpipewire-0.3.so.0`, on every
//! desktop with the portal), so nothing extra is needed to build, and a
//! computer without it only loses Wayland capture. The few structures and
//! the format negotiation (SPA "pods") are written by hand: their sizes and
//! offsets are checked against PipeWire's headers by `pw-layout.c`, in CI.
//!
//! The stream asks for 32-bit RGB in system memory (no DMA-BUF modifiers are
//! offered, so the compositor hands shared memory) and copies each new image
//! out on PipeWire's thread; [`PipeWireCapturer::next_frame`] waits for one.

// FFI with libpipewire throughout: function pointers from dlopen, structures
// it owns, and callbacks it calls on its own thread.
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::fd::{IntoRawFd, OwnedFd};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use libloading::Library;

use crate::{CaptureError, Capturer, Frame, PixelFormat, Rect, even};

// SPA types and keys (spa/utils/type.h, spa/param/...).
const SPA_TYPE_ID: u32 = 3;
const SPA_TYPE_INT: u32 = 4;
const SPA_TYPE_RECTANGLE: u32 = 10;
const SPA_TYPE_FRACTION: u32 = 11;
const SPA_TYPE_OBJECT: u32 = 15;
const SPA_TYPE_CHOICE: u32 = 19;
const SPA_TYPE_OBJECT_FORMAT: u32 = 0x40003;
const SPA_TYPE_OBJECT_PARAM_BUFFERS: u32 = 0x40004;
#[cfg_attr(not(test), allow(dead_code))]
const SPA_CHOICE_NONE: u32 = 0;
const SPA_CHOICE_RANGE: u32 = 1;
const SPA_CHOICE_ENUM: u32 = 3;
const SPA_CHOICE_FLAGS: u32 = 4;
const SPA_PARAM_ENUM_FORMAT: u32 = 3;
const SPA_PARAM_FORMAT: u32 = 4;
const SPA_PARAM_BUFFERS: u32 = 5;
const SPA_PARAM_BUFFERS_DATA_TYPE: u32 = 6;
const SPA_FORMAT_MEDIA_TYPE: u32 = 1;
const SPA_FORMAT_MEDIA_SUBTYPE: u32 = 2;
const SPA_FORMAT_VIDEO_FORMAT: u32 = 0x20001;
const SPA_FORMAT_VIDEO_SIZE: u32 = 0x20003;
const SPA_FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;
const SPA_MEDIA_TYPE_VIDEO: u32 = 2;
const SPA_MEDIA_SUBTYPE_RAW: u32 = 1;
pub(crate) const SPA_VIDEO_FORMAT_RGBX: u32 = 7;
pub(crate) const SPA_VIDEO_FORMAT_BGRX: u32 = 8;
pub(crate) const SPA_VIDEO_FORMAT_RGBA: u32 = 11;
pub(crate) const SPA_VIDEO_FORMAT_BGRA: u32 = 12;
const SPA_DATA_MEM_PTR: u32 = 1;
const SPA_DATA_MEM_FD: u32 = 2;
const SPA_CHUNK_FLAG_CORRUPTED: i32 = 1;

// pipewire/stream.h
const PW_DIRECTION_INPUT: u32 = 0;
const PW_STREAM_FLAG_AUTOCONNECT: u32 = 1 << 0;
const PW_STREAM_FLAG_MAP_BUFFERS: u32 = 1 << 2;
const PW_STREAM_STATE_ERROR: c_int = -1;
const PW_STREAM_STATE_UNCONNECTED: c_int = 0;
const PW_STREAM_STATE_STREAMING: c_int = 3;
const PW_VERSION_STREAM_EVENTS: u32 = 2;

/// How long a new stream may take to start (the compositor answers at once).
const START_WAIT: Duration = Duration::from_secs(10);

// ---- SPA pods: building and reading the few the stream needs ----

/// One pod: its header (body size, type), the body, padding to 8 bytes.
fn pod(kind: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len() + 7);
    out.extend_from_slice(&(body.len() as u32).to_ne_bytes());
    out.extend_from_slice(&kind.to_ne_bytes());
    out.extend_from_slice(body);
    out.resize(out.len().next_multiple_of(8), 0);
    out
}

fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

fn id(value: u32) -> Vec<u8> {
    pod(SPA_TYPE_ID, &value.to_ne_bytes())
}

/// A choice of `kind` among values of `child` type, `size` bytes each (the
/// first one is the default).
fn choice(kind: u32, child: u32, size: u32, values: &[u32]) -> Vec<u8> {
    let mut body = words(&[kind, 0, size, child]);
    body.extend_from_slice(&words(values));
    pod(SPA_TYPE_CHOICE, &body)
}

/// An object: its type, the param it is, and its properties (key, pod).
fn object(kind: u32, param: u32, properties: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut body = words(&[kind, param]);
    for (key, value) in properties {
        body.extend_from_slice(&words(&[*key, 0]));
        body.extend_from_slice(value);
    }
    pod(SPA_TYPE_OBJECT, &body)
}

/// The formats this stream takes: raw video in 32-bit RGB of any size, at up
/// to `fps` frames per second (and less, when nothing changes).
pub(crate) fn enum_format(fps: u32) -> Vec<u8> {
    object(
        SPA_TYPE_OBJECT_FORMAT,
        SPA_PARAM_ENUM_FORMAT,
        &[
            (SPA_FORMAT_MEDIA_TYPE, id(SPA_MEDIA_TYPE_VIDEO)),
            (SPA_FORMAT_MEDIA_SUBTYPE, id(SPA_MEDIA_SUBTYPE_RAW)),
            (
                SPA_FORMAT_VIDEO_FORMAT,
                choice(
                    SPA_CHOICE_ENUM,
                    SPA_TYPE_ID,
                    4,
                    &[
                        SPA_VIDEO_FORMAT_BGRX,
                        SPA_VIDEO_FORMAT_BGRX,
                        SPA_VIDEO_FORMAT_BGRA,
                        SPA_VIDEO_FORMAT_RGBX,
                        SPA_VIDEO_FORMAT_RGBA,
                    ],
                ),
            ),
            (
                SPA_FORMAT_VIDEO_SIZE,
                choice(
                    SPA_CHOICE_RANGE,
                    SPA_TYPE_RECTANGLE,
                    8,
                    &[1920, 1080, 1, 1, 16384, 16384],
                ),
            ),
            (
                SPA_FORMAT_VIDEO_FRAMERATE,
                choice(
                    SPA_CHOICE_RANGE,
                    SPA_TYPE_FRACTION,
                    8,
                    &[fps.max(1), 1, 0, 1, 1000, 1],
                ),
            ),
        ],
    )
}

/// The buffers this stream takes: memory it can read (mapped), no DMA-BUF.
pub(crate) fn buffers() -> Vec<u8> {
    object(
        SPA_TYPE_OBJECT_PARAM_BUFFERS,
        SPA_PARAM_BUFFERS,
        &[(
            SPA_PARAM_BUFFERS_DATA_TYPE,
            choice(
                SPA_CHOICE_FLAGS,
                SPA_TYPE_INT,
                4,
                &[(1 << SPA_DATA_MEM_PTR) | (1 << SPA_DATA_MEM_FD)],
            ),
        )],
    )
}

/// The format both sides agreed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VideoFormat {
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 4)?;
    Some(u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
}

/// Reads the video format out of a `Format` param (a whole pod).
pub(crate) fn parse_format(pod: &[u8]) -> Option<VideoFormat> {
    let size = word(pod, 0)? as usize;
    if word(pod, 4)? != SPA_TYPE_OBJECT || pod.len() < 8 + size {
        return None;
    }
    let body = &pod[8..8 + size];
    let (mut format, mut width, mut height) = (None, None, None);
    // After the object's type and param: key, flags, then a pod.
    let mut at = 8;
    while at + 16 <= body.len() {
        let key = word(body, at)?;
        let value_size = word(body, at + 8)? as usize;
        let value_type = word(body, at + 12)?;
        let value = body.get(at + 16..at + 16 + value_size)?;
        // Settled values come as plain pods or as "none" choices; of a real
        // choice (an offer), the first value is the default.
        let (kind, value) = if value_type == SPA_TYPE_CHOICE {
            (word(value, 12)?, value.get(16..)?)
        } else {
            (value_type, value)
        };
        match key {
            SPA_FORMAT_VIDEO_FORMAT if kind == SPA_TYPE_ID => format = word(value, 0),
            SPA_FORMAT_VIDEO_SIZE if kind == SPA_TYPE_RECTANGLE => {
                width = word(value, 0);
                height = word(value, 4);
            }
            _ => {}
        }
        at += 16 + value_size.next_multiple_of(8);
    }
    Some(VideoFormat {
        format: format?,
        width: width?,
        height: height?,
    })
}

/// Copies one image (`stride` bytes per row from `src`) into tightly packed
/// BGRA of even size, swapping red and blue for the RGB formats.
pub(crate) fn copy_image(
    src: &[u8],
    stride: usize,
    format: VideoFormat,
    out: &mut Vec<u8>,
) -> Option<(u32, u32)> {
    let (width, height) = (even(format.width), even(format.height));
    let row = width as usize * 4;
    if width == 0 || height == 0 || stride < row {
        return None;
    }
    let needed = stride * (height as usize - 1) + row;
    if src.len() < needed {
        return None;
    }
    out.clear();
    out.reserve(row * height as usize);
    let rgb = matches!(format.format, SPA_VIDEO_FORMAT_RGBX | SPA_VIDEO_FORMAT_RGBA);
    for y in 0..height as usize {
        let line = &src[y * stride..y * stride + row];
        if rgb {
            out.extend(line.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]));
        } else {
            out.extend_from_slice(line);
        }
    }
    Some((width, height))
}

// ---- libpipewire ----

#[repr(C)]
struct SpaPod {
    size: u32,
    kind: u32,
}

#[repr(C)]
struct SpaChunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: i32,
}

#[repr(C)]
struct SpaData {
    pub kind: u32,
    pub flags: u32,
    pub fd: i64,
    pub mapoffset: u32,
    pub maxsize: u32,
    pub data: *mut c_void,
    pub chunk: *mut SpaChunk,
}

#[repr(C)]
struct SpaBuffer {
    pub n_metas: u32,
    pub n_datas: u32,
    pub metas: *mut c_void,
    pub datas: *mut SpaData,
}

/// The start of `struct pw_buffer` (only its first field is read).
#[repr(C)]
struct PwBuffer {
    pub buffer: *mut SpaBuffer,
}

/// `struct spa_hook`: a list link, callbacks, a destroy hook and private data.
#[repr(C)]
#[derive(Default)]
struct SpaHook {
    pub link: [usize; 2],
    pub callbacks: [usize; 2],
    pub removed: usize,
    pub private: usize,
}

type Callback = Option<unsafe extern "C" fn(*mut c_void)>;

#[repr(C)]
struct StreamEvents {
    pub version: u32,
    pub destroy: Callback,
    pub state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    pub control_info: Callback,
    pub io_changed: Callback,
    pub param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const SpaPod)>,
    pub add_buffer: Callback,
    pub remove_buffer: Callback,
    pub process: Callback,
    pub drained: Callback,
    pub command: Callback,
    pub trigger_done: Callback,
}

type Ptr = *mut c_void;

struct Pw {
    _lib: Library,
    thread_loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> Ptr,
    thread_loop_get_loop: unsafe extern "C" fn(Ptr) -> Ptr,
    thread_loop_start: unsafe extern "C" fn(Ptr) -> c_int,
    thread_loop_stop: unsafe extern "C" fn(Ptr),
    thread_loop_lock: unsafe extern "C" fn(Ptr),
    thread_loop_unlock: unsafe extern "C" fn(Ptr),
    thread_loop_destroy: unsafe extern "C" fn(Ptr),
    context_new: unsafe extern "C" fn(Ptr, Ptr, usize) -> Ptr,
    context_destroy: unsafe extern "C" fn(Ptr),
    context_connect: unsafe extern "C" fn(Ptr, Ptr, usize) -> Ptr,
    context_connect_fd: unsafe extern "C" fn(Ptr, c_int, Ptr, usize) -> Ptr,
    core_disconnect: unsafe extern "C" fn(Ptr) -> c_int,
    properties_new_string: unsafe extern "C" fn(*const c_char) -> Ptr,
    stream_new: unsafe extern "C" fn(Ptr, *const c_char, Ptr) -> Ptr,
    stream_add_listener: unsafe extern "C" fn(Ptr, *mut SpaHook, *const StreamEvents, Ptr),
    stream_connect: unsafe extern "C" fn(Ptr, u32, u32, u32, *mut *const SpaPod, u32) -> c_int,
    stream_disconnect: unsafe extern "C" fn(Ptr) -> c_int,
    stream_destroy: unsafe extern "C" fn(Ptr),
    stream_dequeue_buffer: unsafe extern "C" fn(Ptr) -> *mut PwBuffer,
    stream_queue_buffer: unsafe extern "C" fn(Ptr, *mut PwBuffer) -> c_int,
    stream_update_params: unsafe extern "C" fn(Ptr, *mut *const SpaPod, u32) -> c_int,
}

// (Plain function pointers into a library that stays loaded.)
unsafe impl Send for Pw {}
unsafe impl Sync for Pw {}

macro_rules! symbol {
    ($lib:expr, $name:literal) => {
        *unsafe { $lib.get(concat!($name, "\0").as_bytes()) }
            .map_err(|e| format!("{}: {e}", $name))?
    };
}

impl Pw {
    fn load() -> Result<Self, String> {
        // SAFETY: loading libpipewire runs only its initializers.
        let lib = unsafe { Library::new("libpipewire-0.3.so.0") }
            .map_err(|e| format!("o PipeWire não está instalado ({e})"))?;
        let init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) = symbol!(lib, "pw_init");
        // SAFETY: pw_init takes no arguments it needs (argc/argv may be null).
        unsafe { init(std::ptr::null_mut(), std::ptr::null_mut()) };
        Ok(Self {
            thread_loop_new: symbol!(lib, "pw_thread_loop_new"),
            thread_loop_get_loop: symbol!(lib, "pw_thread_loop_get_loop"),
            thread_loop_start: symbol!(lib, "pw_thread_loop_start"),
            thread_loop_stop: symbol!(lib, "pw_thread_loop_stop"),
            thread_loop_lock: symbol!(lib, "pw_thread_loop_lock"),
            thread_loop_unlock: symbol!(lib, "pw_thread_loop_unlock"),
            thread_loop_destroy: symbol!(lib, "pw_thread_loop_destroy"),
            context_new: symbol!(lib, "pw_context_new"),
            context_destroy: symbol!(lib, "pw_context_destroy"),
            context_connect: symbol!(lib, "pw_context_connect"),
            context_connect_fd: symbol!(lib, "pw_context_connect_fd"),
            core_disconnect: symbol!(lib, "pw_core_disconnect"),
            properties_new_string: symbol!(lib, "pw_properties_new_string"),
            stream_new: symbol!(lib, "pw_stream_new"),
            stream_add_listener: symbol!(lib, "pw_stream_add_listener"),
            stream_connect: symbol!(lib, "pw_stream_connect"),
            stream_disconnect: symbol!(lib, "pw_stream_disconnect"),
            stream_destroy: symbol!(lib, "pw_stream_destroy"),
            stream_dequeue_buffer: symbol!(lib, "pw_stream_dequeue_buffer"),
            stream_queue_buffer: symbol!(lib, "pw_stream_queue_buffer"),
            stream_update_params: symbol!(lib, "pw_stream_update_params"),
            _lib: lib,
        })
    }
}

fn pw() -> Result<&'static Pw, CaptureError> {
    static PW: OnceLock<Result<Pw, String>> = OnceLock::new();
    PW.get_or_init(Pw::load)
        .as_ref()
        .map_err(|e| CaptureError::Platform(e.clone()))
}

/// Where the stream's PipeWire is.
pub(crate) enum Remote {
    /// The portal's: only the nodes it shared are there.
    Fd(OwnedFd),
    /// This user's own daemon (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    Default,
}

/// What PipeWire's thread hands to the capturer.
#[derive(Default)]
struct State {
    format: Option<VideoFormat>,
    /// The newest image, in BGRA, and its size.
    image: Vec<u8>,
    size: (u32, u32),
    seq: u64,
    copy_time: Duration,
    streaming: bool,
    error: Option<String>,
}

struct Shared {
    pw: &'static Pw,
    stream: AtomicPtr<c_void>,
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().expect("lock"));
        self.changed.notify_all();
    }
}

unsafe extern "C" fn on_state_changed(data: Ptr, _old: c_int, state: c_int, error: *const c_char) {
    // SAFETY: `data` is the capturer's `Shared`, alive while the stream is.
    let shared = unsafe { &*(data as *const Shared) };
    let message = (!error.is_null())
        // SAFETY: PipeWire passes a C string, or null.
        .then(|| {
            unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned()
        });
    shared.update(|s| match state {
        PW_STREAM_STATE_ERROR => {
            s.error = Some(message.unwrap_or_else(|| "erro no fluxo do PipeWire".into()))
        }
        PW_STREAM_STATE_UNCONNECTED if s.streaming => {
            s.error = Some("o compartilhamento da tela terminou".into())
        }
        PW_STREAM_STATE_STREAMING => s.streaming = true,
        _ => {}
    });
}

unsafe extern "C" fn on_param_changed(data: Ptr, id: u32, param: *const SpaPod) {
    if id != SPA_PARAM_FORMAT || param.is_null() {
        return;
    }
    // SAFETY: `data` is the capturer's `Shared`; `param` is a whole pod,
    // its header telling the size of what follows.
    let (shared, pod) = unsafe {
        let size = (*param).size as usize;
        (
            &*(data as *const Shared),
            std::slice::from_raw_parts(param as *const u8, 8 + size),
        )
    };
    let Some(format) = parse_format(pod) else {
        tracing::warn!("PipeWire: formato de vídeo não reconhecido");
        return;
    };
    tracing::debug!(
        "PipeWire: {}x{}, formato {}",
        format.width,
        format.height,
        format.format
    );
    shared.update(|s| s.format = Some(format));
    let params = buffers();
    let mut list = [params.as_ptr() as *const SpaPod];
    let stream = shared.stream.load(Ordering::Acquire);
    // SAFETY: on PipeWire's thread, inside its callback, with a valid pod.
    unsafe { (shared.pw.stream_update_params)(stream, list.as_mut_ptr(), 1) };
}

unsafe extern "C" fn on_process(data: Ptr) {
    // SAFETY: `data` is the capturer's `Shared`.
    let shared = unsafe { &*(data as *const Shared) };
    let pw = shared.pw;
    let stream = shared.stream.load(Ordering::Acquire);
    // Only the newest buffer matters: older ones go straight back.
    let mut newest: *mut PwBuffer = std::ptr::null_mut();
    loop {
        // SAFETY: on PipeWire's thread, in the process callback.
        let next = unsafe { (pw.stream_dequeue_buffer)(stream) };
        if next.is_null() {
            break;
        }
        if !newest.is_null() {
            // SAFETY: a buffer this stream gave out.
            unsafe { (pw.stream_queue_buffer)(stream, newest) };
        }
        newest = next;
    }
    if newest.is_null() {
        return;
    }
    let started = Instant::now();
    let mut state = shared.state.lock().expect("lock");
    if let Some(format) = state.format {
        // SAFETY: a buffer the stream mapped (MAP_BUFFERS), with its data
        // and chunk valid until it goes back to the stream.
        let image = unsafe { buffer_image(newest) };
        if let Some((src, stride)) = image {
            let mut out = std::mem::take(&mut state.image);
            if let Some(size) = copy_image(src, stride, format, &mut out) {
                state.size = size;
                state.seq += 1;
                state.copy_time = started.elapsed();
            }
            state.image = out;
        }
    }
    drop(state);
    shared.changed.notify_all();
    // SAFETY: back to the stream it came from.
    unsafe { (pw.stream_queue_buffer)(stream, newest) };
}

/// The pixels of a buffer, and their stride; `None` for one with no image
/// (the compositor sends those when only the pointer moved).
///
/// # Safety
/// `buffer` must be a mapped buffer dequeued from a stream and not given back.
unsafe fn buffer_image<'a>(buffer: *mut PwBuffer) -> Option<(&'a [u8], usize)> {
    // SAFETY: by this function's contract.
    unsafe {
        let spa = (*buffer).buffer;
        if spa.is_null() || (*spa).n_datas == 0 || (*spa).datas.is_null() {
            return None;
        }
        let data = &*(*spa).datas;
        if data.data.is_null() || data.chunk.is_null() {
            return None;
        }
        let chunk = &*data.chunk;
        if chunk.size == 0 || chunk.flags & SPA_CHUNK_FLAG_CORRUPTED != 0 {
            return None;
        }
        let offset = (chunk.offset as usize).min(data.maxsize as usize);
        let len = (chunk.size as usize).min(data.maxsize as usize - offset);
        let bytes = std::slice::from_raw_parts((data.data as *const u8).add(offset), len);
        Some((bytes, chunk.stride.max(0) as usize))
    }
}

/// Captures one PipeWire video node.
pub(crate) struct PipeWireCapturer {
    shared: Arc<Shared>,
    thread_loop: Ptr,
    context: Ptr,
    core: Ptr,
    stream: Ptr,
    _hook: Box<SpaHook>,
    _events: Box<StreamEvents>,
    rect: Rect,
    frame: Option<Frame>,
    seen: u64,
    copy_time: Duration,
    interval: Duration,
    next_at: Instant,
}

impl PipeWireCapturer {
    /// Connects to `node` on `remote`; `rect` is where it sits among the
    /// displays.
    pub(crate) fn new(
        remote: Remote,
        node: u32,
        rect: Rect,
        max_fps: u32,
    ) -> Result<Self, CaptureError> {
        let pw = pw()?;
        let failed = |what: &str| CaptureError::Platform(format!("PipeWire: {what}"));
        let shared = Arc::new(Shared {
            pw,
            stream: AtomicPtr::new(std::ptr::null_mut()),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let mut hook = Box::<SpaHook>::default();
        let events = Box::new(StreamEvents {
            version: PW_VERSION_STREAM_EVENTS,
            destroy: None,
            state_changed: Some(on_state_changed),
            control_info: None,
            io_changed: None,
            param_changed: Some(on_param_changed),
            add_buffer: None,
            remove_buffer: None,
            process: Some(on_process),
            drained: None,
            command: None,
            trigger_done: None,
        });
        // SAFETY: the usual PipeWire sequence; every object is checked and,
        // from here on, owned by the capturer (Drop undoes it in order).
        unsafe {
            let thread_loop = (pw.thread_loop_new)(c"hext-pipewire".as_ptr(), std::ptr::null());
            if thread_loop.is_null() {
                return Err(failed("sem thread"));
            }
            let context = (pw.context_new)(
                (pw.thread_loop_get_loop)(thread_loop),
                std::ptr::null_mut(),
                0,
            );
            if context.is_null() {
                (pw.thread_loop_destroy)(thread_loop);
                return Err(failed("sem contexto"));
            }
            let mut capturer = Self {
                shared,
                thread_loop,
                context,
                core: std::ptr::null_mut(),
                stream: std::ptr::null_mut(),
                _hook: Box::default(),
                _events: events,
                rect,
                frame: None,
                seen: 0,
                copy_time: Duration::ZERO,
                interval: Duration::from_secs(1) / max_fps.max(1),
                next_at: Instant::now(),
            };
            if (pw.thread_loop_start)(thread_loop) < 0 {
                return Err(failed("a thread não abriu"));
            }
            (pw.thread_loop_lock)(thread_loop);
            capturer.core = match remote {
                Remote::Fd(fd) => {
                    (pw.context_connect_fd)(context, fd.into_raw_fd(), std::ptr::null_mut(), 0)
                }
                Remote::Default => (pw.context_connect)(context, std::ptr::null_mut(), 0),
            };
            if capturer.core.is_null() {
                (pw.thread_loop_unlock)(thread_loop);
                return Err(failed("sem conexão"));
            }
            let props = (pw.properties_new_string)(
                c"media.type=Video media.category=Capture media.role=Screen".as_ptr(),
            );
            capturer.stream = (pw.stream_new)(capturer.core, c"Hext Remote".as_ptr(), props);
            if capturer.stream.is_null() {
                (pw.thread_loop_unlock)(thread_loop);
                return Err(failed("sem fluxo"));
            }
            capturer
                .shared
                .stream
                .store(capturer.stream, Ordering::Release);
            (pw.stream_add_listener)(
                capturer.stream,
                &mut *hook,
                &*capturer._events,
                Arc::as_ptr(&capturer.shared) as Ptr,
            );
            capturer._hook = hook;
            let params = enum_format(max_fps);
            let mut list = [params.as_ptr() as *const SpaPod];
            let connected = (pw.stream_connect)(
                capturer.stream,
                PW_DIRECTION_INPUT,
                node,
                PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS,
                list.as_mut_ptr(),
                1,
            );
            (pw.thread_loop_unlock)(thread_loop);
            if connected < 0 {
                return Err(failed(&format!("o fluxo não conectou ({connected})")));
            }
            capturer.wait_start()?;
            tracing::info!(
                "capturando o monitor pelo PipeWire (Wayland): nó {node}, {}x{} em ({}, {})",
                rect.width,
                rect.height,
                rect.x,
                rect.y
            );
            Ok(capturer)
        }
    }

    /// Waits for the stream to flow, or to fail.
    fn wait_start(&self) -> Result<(), CaptureError> {
        let deadline = Instant::now() + START_WAIT;
        let mut state = self.shared.state.lock().expect("lock");
        loop {
            if let Some(error) = &state.error {
                return Err(CaptureError::Platform(format!("PipeWire: {error}")));
            }
            if state.streaming {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(CaptureError::Platform(
                    "PipeWire: o fluxo da tela não começou".into(),
                ));
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .expect("lock")
                .0;
        }
    }
}

impl Capturer for PipeWireCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<bool, CaptureError> {
        let now = Instant::now();
        let deadline = now + timeout;
        // Never more often than the frame rate asks.
        if self.next_at > now {
            let wait = self.next_at - now;
            if wait >= timeout {
                std::thread::sleep(timeout);
                return Ok(false);
            }
            std::thread::sleep(wait);
        }
        let mut state = self.shared.state.lock().expect("lock");
        loop {
            if let Some(error) = &state.error {
                return Err(CaptureError::DisplayGone(error.clone()));
            }
            if state.seq != self.seen {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .expect("lock")
                .0;
        }
        self.seen = state.seq;
        self.copy_time = state.copy_time;
        let (width, height) = state.size;
        // The two buffers trade places: no allocation per frame.
        let spare = self.frame.take().map(|f| f.data).unwrap_or_default();
        let data = std::mem::replace(&mut state.image, spare);
        drop(state);
        self.frame = Some(Frame {
            width,
            height,
            format: PixelFormat::Bgra,
            data,
        });
        let started = Instant::now();
        self.next_at = self.next_at.max(started - self.interval) + self.interval;
        Ok(true)
    }

    fn frame(&self) -> Option<&Frame> {
        self.frame.as_ref()
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn copy_time(&self) -> Duration {
        self.copy_time
    }
}

impl Drop for PipeWireCapturer {
    fn drop(&mut self) {
        let pw = self.shared.pw;
        // SAFETY: tearing down what `new` made, in reverse, with PipeWire's
        // thread held while the stream and the core go, then stopped.
        unsafe {
            (pw.thread_loop_lock)(self.thread_loop);
            if !self.stream.is_null() {
                (pw.stream_disconnect)(self.stream);
                (pw.stream_destroy)(self.stream);
            }
            if !self.core.is_null() {
                (pw.core_disconnect)(self.core);
            }
            (pw.thread_loop_unlock)(self.thread_loop);
            (pw.thread_loop_stop)(self.thread_loop);
            (pw.context_destroy)(self.context);
            (pw.thread_loop_destroy)(self.thread_loop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Format` param as a compositor sends it: fixed values, some as
    /// plain pods and some as "none" choices.
    fn negotiated(format: u32, width: u32, height: u32) -> Vec<u8> {
        object(
            SPA_TYPE_OBJECT_FORMAT,
            SPA_PARAM_FORMAT,
            &[
                (SPA_FORMAT_MEDIA_TYPE, id(SPA_MEDIA_TYPE_VIDEO)),
                (SPA_FORMAT_MEDIA_SUBTYPE, id(SPA_MEDIA_SUBTYPE_RAW)),
                (SPA_FORMAT_VIDEO_FORMAT, id(format)),
                (
                    SPA_FORMAT_VIDEO_SIZE,
                    choice(SPA_CHOICE_NONE, SPA_TYPE_RECTANGLE, 8, &[width, height]),
                ),
                (
                    SPA_FORMAT_VIDEO_FRAMERATE,
                    pod(SPA_TYPE_FRACTION, &words(&[0, 1])),
                ),
            ],
        )
    }

    #[test]
    fn pods_are_padded_and_sized() {
        let p = id(7);
        assert_eq!(p.len(), 16);
        assert_eq!(word(&p, 0), Some(4));
        assert_eq!(word(&p, 4), Some(SPA_TYPE_ID));
        assert_eq!(word(&p, 8), Some(7));
        let offer = enum_format(60);
        assert_eq!(offer.len() % 8, 0);
        assert_eq!(word(&offer, 0).unwrap() as usize + 8, offer.len());
        assert_eq!(word(&offer, 8), Some(SPA_TYPE_OBJECT_FORMAT));
        assert_eq!(word(&offer, 12), Some(SPA_PARAM_ENUM_FORMAT));
        assert_eq!(buffers().len() % 8, 0);
    }

    #[test]
    fn reads_the_agreed_format() {
        let format = parse_format(&negotiated(SPA_VIDEO_FORMAT_BGRX, 2560, 1440)).unwrap();
        assert_eq!(
            format,
            VideoFormat {
                format: SPA_VIDEO_FORMAT_BGRX,
                width: 2560,
                height: 1440
            }
        );
        // The offer itself parses too: the default of each choice.
        let offer = parse_format(&enum_format(30)).unwrap();
        assert_eq!(
            (offer.format, offer.width, offer.height),
            (SPA_VIDEO_FORMAT_BGRX, 1920, 1080)
        );
        // Cut short, or not an object: nothing.
        let whole = negotiated(SPA_VIDEO_FORMAT_RGBX, 10, 10);
        assert_eq!(parse_format(&whole[..whole.len() - 8]), None);
        assert_eq!(parse_format(&id(3)), None);
    }

    #[test]
    fn images_come_out_as_even_bgra() {
        // 3x3 pixels, a stride of 16 bytes (4 of padding per row).
        let mut src = Vec::new();
        for y in 0..3u8 {
            for x in 0..3u8 {
                src.extend_from_slice(&[x, y, 100, 255]);
            }
            src.extend_from_slice(&[0; 4]);
        }
        let mut out = Vec::new();
        let bgrx = VideoFormat {
            format: SPA_VIDEO_FORMAT_BGRX,
            width: 3,
            height: 3,
        };
        assert_eq!(copy_image(&src, 16, bgrx, &mut out), Some((2, 2)));
        assert_eq!(
            out,
            [
                0, 0, 100, 255, 1, 0, 100, 255, 0, 1, 100, 255, 1, 1, 100, 255
            ]
        );
        let rgbx = VideoFormat {
            format: SPA_VIDEO_FORMAT_RGBX,
            ..bgrx
        };
        copy_image(&src, 16, rgbx, &mut out).unwrap();
        assert_eq!(&out[..4], &[100, 0, 0, 255]);
        // Too little data, or a stride shorter than a row: nothing.
        assert_eq!(copy_image(&src[..20], 16, bgrx, &mut out), None);
        assert_eq!(copy_image(&src, 4, bgrx, &mut out), None);
    }

    /// The structures written here against PipeWire's headers (CI:
    /// `pw-layout.c`, with libpipewire-0.3-dev).
    #[test]
    #[ignore = "needs HEXT_PW_LAYOUT, from pw-layout.c"]
    fn layouts_match_pipewire() {
        use std::mem::offset_of;
        let path = std::env::var("HEXT_PW_LAYOUT").expect("HEXT_PW_LAYOUT");
        let text = std::fs::read_to_string(path).unwrap();
        let ours: &[(&str, usize)] = &[
            ("spa_hook", size_of::<SpaHook>()),
            ("pw_stream_events", size_of::<StreamEvents>()),
            (
                "pw_stream_events.state_changed",
                offset_of!(StreamEvents, state_changed),
            ),
            (
                "pw_stream_events.param_changed",
                offset_of!(StreamEvents, param_changed),
            ),
            (
                "pw_stream_events.process",
                offset_of!(StreamEvents, process),
            ),
            ("spa_buffer", size_of::<SpaBuffer>()),
            ("spa_buffer.datas", offset_of!(SpaBuffer, datas)),
            ("spa_data", size_of::<SpaData>()),
            ("spa_data.maxsize", offset_of!(SpaData, maxsize)),
            ("spa_data.data", offset_of!(SpaData, data)),
            ("spa_data.chunk", offset_of!(SpaData, chunk)),
            ("spa_chunk", size_of::<SpaChunk>()),
            ("spa_chunk.stride", offset_of!(SpaChunk, stride)),
            ("pw_buffer.buffer", offset_of!(PwBuffer, buffer)),
        ];
        let constants: &[(&str, i64)] = &[
            ("PW_VERSION_STREAM_EVENTS", PW_VERSION_STREAM_EVENTS.into()),
            (
                "PW_STREAM_FLAG_AUTOCONNECT",
                PW_STREAM_FLAG_AUTOCONNECT.into(),
            ),
            (
                "PW_STREAM_FLAG_MAP_BUFFERS",
                PW_STREAM_FLAG_MAP_BUFFERS.into(),
            ),
            ("PW_STREAM_STATE_ERROR", PW_STREAM_STATE_ERROR.into()),
            (
                "PW_STREAM_STATE_UNCONNECTED",
                PW_STREAM_STATE_UNCONNECTED.into(),
            ),
            (
                "PW_STREAM_STATE_STREAMING",
                PW_STREAM_STATE_STREAMING.into(),
            ),
            ("PW_DIRECTION_INPUT", PW_DIRECTION_INPUT.into()),
            ("SPA_TYPE_Id", SPA_TYPE_ID.into()),
            ("SPA_TYPE_Int", SPA_TYPE_INT.into()),
            ("SPA_TYPE_Rectangle", SPA_TYPE_RECTANGLE.into()),
            ("SPA_TYPE_Fraction", SPA_TYPE_FRACTION.into()),
            ("SPA_TYPE_Object", SPA_TYPE_OBJECT.into()),
            ("SPA_TYPE_Choice", SPA_TYPE_CHOICE.into()),
            ("SPA_TYPE_OBJECT_Format", SPA_TYPE_OBJECT_FORMAT.into()),
            (
                "SPA_TYPE_OBJECT_ParamBuffers",
                SPA_TYPE_OBJECT_PARAM_BUFFERS.into(),
            ),
            ("SPA_CHOICE_None", SPA_CHOICE_NONE.into()),
            ("SPA_CHOICE_Range", SPA_CHOICE_RANGE.into()),
            ("SPA_CHOICE_Enum", SPA_CHOICE_ENUM.into()),
            ("SPA_CHOICE_Flags", SPA_CHOICE_FLAGS.into()),
            ("SPA_PARAM_EnumFormat", SPA_PARAM_ENUM_FORMAT.into()),
            ("SPA_PARAM_Format", SPA_PARAM_FORMAT.into()),
            ("SPA_PARAM_Buffers", SPA_PARAM_BUFFERS.into()),
            (
                "SPA_PARAM_BUFFERS_dataType",
                SPA_PARAM_BUFFERS_DATA_TYPE.into(),
            ),
            ("SPA_FORMAT_mediaType", SPA_FORMAT_MEDIA_TYPE.into()),
            ("SPA_FORMAT_mediaSubtype", SPA_FORMAT_MEDIA_SUBTYPE.into()),
            ("SPA_FORMAT_VIDEO_format", SPA_FORMAT_VIDEO_FORMAT.into()),
            ("SPA_FORMAT_VIDEO_size", SPA_FORMAT_VIDEO_SIZE.into()),
            (
                "SPA_FORMAT_VIDEO_framerate",
                SPA_FORMAT_VIDEO_FRAMERATE.into(),
            ),
            ("SPA_MEDIA_TYPE_video", SPA_MEDIA_TYPE_VIDEO.into()),
            ("SPA_MEDIA_SUBTYPE_raw", SPA_MEDIA_SUBTYPE_RAW.into()),
            ("SPA_VIDEO_FORMAT_RGBx", SPA_VIDEO_FORMAT_RGBX.into()),
            ("SPA_VIDEO_FORMAT_BGRx", SPA_VIDEO_FORMAT_BGRX.into()),
            ("SPA_VIDEO_FORMAT_RGBA", SPA_VIDEO_FORMAT_RGBA.into()),
            ("SPA_VIDEO_FORMAT_BGRA", SPA_VIDEO_FORMAT_BGRA.into()),
            ("SPA_DATA_MemPtr", SPA_DATA_MEM_PTR.into()),
            ("SPA_DATA_MemFd", SPA_DATA_MEM_FD.into()),
            ("SPA_CHUNK_FLAG_CORRUPTED", SPA_CHUNK_FLAG_CORRUPTED.into()),
        ];
        let theirs = |name: &str| -> i64 {
            text.lines()
                .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.trim().parse().ok())
                .unwrap_or_else(|| panic!("{name} missing from the layout file"))
        };
        for (name, value) in ours {
            assert_eq!(*value as i64, theirs(name), "{name}");
        }
        for (name, value) in constants {
            assert_eq!(*value, theirs(name), "{name}");
        }
    }

    /// A real PipeWire stream: a test source (`pipewire-source.sh`, with
    /// GStreamer's SMPTE bars) comes out as BGRA, the right way round.
    #[test]
    #[ignore = "needs a PipeWire daemon and HEXT_TEST_PIPEWIRE_NODE (pipewire-source.sh)"]
    fn pipewire_captures_a_test_source() {
        let node: u32 = std::env::var("HEXT_TEST_PIPEWIRE_NODE")
            .expect("HEXT_TEST_PIPEWIRE_NODE")
            .parse()
            .unwrap();
        let rect = Rect {
            x: 0,
            y: 0,
            width: 320,
            height: 240,
        };
        let mut capturer = PipeWireCapturer::new(Remote::Default, node, rect, 30).unwrap();
        assert!(
            capturer.next_frame(Duration::from_secs(10)).unwrap(),
            "no frame"
        );
        let frame = capturer.frame().unwrap();
        assert_eq!((frame.width, frame.height), (320, 240));
        assert_eq!(frame.data.len(), 320 * 240 * 4);
        let at = |x: usize, y: usize| {
            let i = (y * 320 + x) * 4;
            [frame.data[i], frame.data[i + 1], frame.data[i + 2]]
        };
        // SMPTE bars, in BGRA: white, yellow (no blue), cyan (no red).
        let [b, g, r] = at(10, 10);
        assert!(b > 150 && g > 150 && r > 150, "first bar: {:?}", at(10, 10));
        let [b, g, r] = at(68, 10);
        assert!(b < 60 && g > 150 && r > 150, "second bar: {:?}", at(68, 10));
        let [b, g, r] = at(114, 10);
        assert!(b > 150 && g > 150 && r < 60, "third bar: {:?}", at(114, 10));
        // A live source: more frames come.
        assert!(capturer.next_frame(Duration::from_secs(5)).unwrap());
    }
}
