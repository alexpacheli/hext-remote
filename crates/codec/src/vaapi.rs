//! H.264 on the graphics card on Linux, through VA-API (Intel and AMD;
//! F7, milestone 4).
//!
//! libva is opened at run time (`libva.so.2`, `libva-drm.so.2`): the
//! package only recommends it, and without it (or without a card that
//! encodes H.264 Constrained Baseline) the processor encodes. Each frame is
//! one IDR or P slice, CAVLC, one reference, no B-frames: what OpenH264
//! decodes in the viewer. The driver writes the headers (SPS, PPS, slice).
//!
//! Before it is used, the encoder encodes a few test frames and OpenH264
//! decodes them: a driver whose output the viewer would not show is left
//! out right there, and the processor encodes instead.

// FFI to libva, opened with dlopen: each call is a plain C call, and the
// structures below follow libva's headers (checked by the `layouts` test
// against them, in CI).
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::os::fd::{AsRawFd, OwnedFd};
use std::ptr;

use bytemuck::{Pod, Zeroable};
use libloading::Library;

use crate::{
    CodecError, Encoded, EncoderSettings, Image, PixelFormat, VideoEncoder, check_frame, h264,
};

type Display = *mut c_void;
type Status = c_int;
type Id = c_uint;

const SUCCESS: Status = 0;
const INVALID_ID: Id = 0xffff_ffff;

const PROFILE_H264_CONSTRAINED_BASELINE: c_int = 13;
const ENTRYPOINT_ENC_SLICE: c_int = 6;
const ENTRYPOINT_ENC_SLICE_LP: c_int = 8;

const ATTRIB_RT_FORMAT: c_int = 0;
const ATTRIB_RATE_CONTROL: c_int = 5;
const RT_FORMAT_YUV420: u32 = 0x1;
const RC_CBR: u32 = 0x2;
const ATTRIB_NOT_SUPPORTED: u32 = 0x8000_0000;

const PROGRESSIVE: c_int = 0x1;
const FOURCC_NV12: u32 = 0x3231_564E;
const LSB_FIRST: u32 = 1;

const BUFFER_CODED: c_int = 21;
const BUFFER_SEQUENCE: c_int = 22;
const BUFFER_PICTURE: c_int = 23;
const BUFFER_SLICE: c_int = 24;
const BUFFER_MISC: c_int = 27;

const MISC_FRAME_RATE: u32 = 0;
const MISC_RATE_CONTROL: u32 = 1;
const MISC_HRD: u32 = 5;

const PICTURE_INVALID: u32 = 0x1;
const PICTURE_SHORT_TERM_REFERENCE: u32 = 0x8;

const SLICE_P: u8 = 0;
const SLICE_I: u8 = 2;

/// `log2_max_frame_num` and `log2_max_pic_order_cnt_lsb`: frame numbers
/// and picture orders count up to 2^16, and an IDR comes before that.
const LOG2_MAX: u32 = 16;
const IDR_EVERY: u64 = 1 << 15;

// ------------------------------------------------------------- structures

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod)]
pub(crate) struct ConfigAttrib {
    kind: c_int,
    value: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod)]
pub(crate) struct PictureH264 {
    picture_id: Id,
    frame_idx: u32,
    flags: u32,
    top_field_order_cnt: i32,
    bottom_field_order_cnt: i32,
    reserved: [u32; 4],
}

impl PictureH264 {
    const INVALID: Self = Self {
        picture_id: INVALID_ID,
        frame_idx: 0,
        flags: PICTURE_INVALID,
        top_field_order_cnt: 0,
        bottom_field_order_cnt: 0,
        reserved: [0; 4],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable)]
pub(crate) struct SequenceH264 {
    seq_parameter_set_id: u8,
    level_idc: u8,
    intra_period: u32,
    intra_idr_period: u32,
    ip_period: u32,
    bits_per_second: u32,
    max_num_ref_frames: u32,
    picture_width_in_mbs: u16,
    picture_height_in_mbs: u16,
    seq_fields: u32,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
    num_ref_frames_in_pic_order_cnt_cycle: u8,
    offset_for_non_ref_pic: i32,
    offset_for_top_to_bottom_field: i32,
    offset_for_ref_frame: [i32; 256],
    frame_cropping_flag: u8,
    frame_crop_left_offset: u32,
    frame_crop_right_offset: u32,
    frame_crop_top_offset: u32,
    frame_crop_bottom_offset: u32,
    vui_parameters_present_flag: u8,
    vui_fields: u32,
    aspect_ratio_idc: u8,
    sar_width: u32,
    sar_height: u32,
    num_units_in_tick: u32,
    time_scale: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable)]
pub(crate) struct PictureParamH264 {
    curr_pic: PictureH264,
    reference_frames: [PictureH264; 16],
    coded_buf: Id,
    pic_parameter_set_id: u8,
    seq_parameter_set_id: u8,
    last_picture: u8,
    frame_num: u16,
    pic_init_qp: u8,
    num_ref_idx_l0_active_minus1: u8,
    num_ref_idx_l1_active_minus1: u8,
    chroma_qp_index_offset: i8,
    second_chroma_qp_index_offset: i8,
    pic_fields: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable)]
pub(crate) struct SliceParamH264 {
    macroblock_address: u32,
    num_macroblocks: u32,
    macroblock_info: Id,
    slice_type: u8,
    pic_parameter_set_id: u8,
    idr_pic_id: u16,
    pic_order_cnt_lsb: u16,
    delta_pic_order_cnt_bottom: i32,
    delta_pic_order_cnt: [i32; 2],
    direct_spatial_mv_pred_flag: u8,
    num_ref_idx_active_override_flag: u8,
    num_ref_idx_l0_active_minus1: u8,
    num_ref_idx_l1_active_minus1: u8,
    ref_pic_list0: [PictureH264; 32],
    ref_pic_list1: [PictureH264; 32],
    luma_log2_weight_denom: u8,
    chroma_log2_weight_denom: u8,
    luma_weight_l0_flag: u8,
    luma_weight_l0: [i16; 32],
    luma_offset_l0: [i16; 32],
    chroma_weight_l0_flag: u8,
    chroma_weight_l0: [[i16; 2]; 32],
    chroma_offset_l0: [[i16; 2]; 32],
    luma_weight_l1_flag: u8,
    luma_weight_l1: [i16; 32],
    luma_offset_l1: [i16; 32],
    chroma_weight_l1_flag: u8,
    chroma_weight_l1: [[i16; 2]; 32],
    chroma_offset_l1: [[i16; 2]; 32],
    cabac_init_idc: u8,
    slice_qp_delta: i8,
    disable_deblocking_filter_idc: u8,
    slice_alpha_c0_offset_div2: i8,
    slice_beta_offset_div2: i8,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod)]
pub(crate) struct MiscRateControl {
    bits_per_second: u32,
    target_percentage: u32,
    window_size: u32,
    initial_qp: u32,
    min_qp: u32,
    basic_unit_size: u32,
    rc_flags: u32,
    icq_quality_factor: u32,
    max_qp: u32,
    quality_factor: u32,
    target_frame_size: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod)]
pub(crate) struct MiscFrameRate {
    framerate: u32,
    framerate_flags: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod)]
pub(crate) struct MiscHrd {
    initial_buffer_fullness: u32,
    buffer_size: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod)]
pub(crate) struct ImageFormat {
    fourcc: u32,
    byte_order: u32,
    bits_per_pixel: u32,
    depth: u32,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    alpha_mask: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Zeroable, Pod)]
pub(crate) struct VaImage {
    image_id: Id,
    format: ImageFormat,
    buf: Id,
    width: u16,
    height: u16,
    data_size: u32,
    num_planes: u32,
    pitches: [u32; 3],
    offsets: [u32; 3],
    num_palette_entries: i32,
    entry_bytes: i32,
    component_order: [i8; 4],
    reserved: [u32; 4],
}

#[repr(C)]
pub(crate) struct CodedSegment {
    size: u32,
    bit_offset: u32,
    status: u32,
    reserved: u32,
    buf: *mut c_void,
    next: *mut c_void,
    va_reserved: [u32; 4],
}

/// Bit fields of the sequence: 4:2:0, frames only, 8x8 inference, frame
/// numbers and picture orders (type 0) of [`LOG2_MAX`] bits.
pub(crate) fn seq_fields() -> u32 {
    let chroma_format_idc = 1;
    let frame_mbs_only = 1 << 2;
    let direct_8x8_inference = 1 << 5;
    let log2_max_frame_num_minus4 = (LOG2_MAX - 4) << 6;
    let pic_order_cnt_type = 0 << 10;
    let log2_max_poc_lsb_minus4 = (LOG2_MAX - 4) << 12;
    chroma_format_idc
        | frame_mbs_only
        | direct_8x8_inference
        | log2_max_frame_num_minus4
        | pic_order_cnt_type
        | log2_max_poc_lsb_minus4
}

/// Bit fields of a picture: IDR or not, used as reference, CAVLC, the
/// deblocking filter's control in the slice header.
pub(crate) fn pic_fields(idr: bool) -> u32 {
    let idr_pic = u32::from(idr);
    let reference_pic = 1 << 1;
    let deblocking_filter_control_present = 1 << 9;
    idr_pic | reference_pic | deblocking_filter_control_present
}

/// Macroblocks across and down for a picture, and the cropping (in the
/// 2-pixel units of 4:2:0) of what is past its right and bottom edges.
pub(crate) fn macroblocks(width: u32, height: u32) -> ((u16, u16), (u32, u32)) {
    let (mw, mh) = (width.div_ceil(16), height.div_ceil(16));
    (
        (mw as u16, mh as u16),
        ((mw * 16 - width) / 2, (mh * 16 - height) / 2),
    )
}

// ---------------------------------------------------------------- library

type Fn0 = unsafe extern "C" fn(Display) -> Status;

/// libva's functions, from the libraries kept open here.
struct Va {
    _va: Library,
    _drm: Library,
    display: Display,
    /// The render node the display uses.
    _node: OwnedFd,
    vendor: String,
    entrypoint: c_int,
    terminate: Fn0,
    error_str: unsafe extern "C" fn(Status) -> *const c_char,
    create_config:
        unsafe extern "C" fn(Display, c_int, c_int, *mut ConfigAttrib, c_int, *mut Id) -> Status,
    destroy_config: unsafe extern "C" fn(Display, Id) -> Status,
    create_surfaces: unsafe extern "C" fn(
        Display,
        c_uint,
        c_uint,
        c_uint,
        *mut Id,
        c_uint,
        *mut c_void,
        c_uint,
    ) -> Status,
    destroy_surfaces: unsafe extern "C" fn(Display, *mut Id, c_int) -> Status,
    create_context:
        unsafe extern "C" fn(Display, Id, c_int, c_int, c_int, *mut Id, c_int, *mut Id) -> Status,
    destroy_context: unsafe extern "C" fn(Display, Id) -> Status,
    create_buffer:
        unsafe extern "C" fn(Display, Id, c_int, c_uint, c_uint, *mut c_void, *mut Id) -> Status,
    destroy_buffer: unsafe extern "C" fn(Display, Id) -> Status,
    map_buffer: unsafe extern "C" fn(Display, Id, *mut *mut c_void) -> Status,
    unmap_buffer: unsafe extern "C" fn(Display, Id) -> Status,
    create_image:
        unsafe extern "C" fn(Display, *mut ImageFormat, c_int, c_int, *mut VaImage) -> Status,
    destroy_image: unsafe extern "C" fn(Display, Id) -> Status,
    put_image: unsafe extern "C" fn(
        Display,
        Id,
        Id,
        c_int,
        c_int,
        c_uint,
        c_uint,
        c_int,
        c_int,
        c_uint,
        c_uint,
    ) -> Status,
    begin_picture: unsafe extern "C" fn(Display, Id, Id) -> Status,
    render_picture: unsafe extern "C" fn(Display, Id, *mut Id, c_int) -> Status,
    end_picture: unsafe extern "C" fn(Display, Id) -> Status,
    sync_surface: unsafe extern "C" fn(Display, Id) -> Status,
}

fn hardware(what: impl std::fmt::Display) -> CodecError {
    CodecError::Hardware(format!("VA-API: {what}"))
}

/// Takes a function out of `lib`, or says which one is missing.
macro_rules! symbol {
    ($lib:expr, $name:literal) => {
        *unsafe { $lib.get(concat!($name, "\0").as_bytes()) }
            .map_err(|e| hardware(format!("{}: {e}", $name)))?
    };
}

impl Va {
    /// Opens libva on the first render node whose driver encodes H.264
    /// Constrained Baseline at CBR.
    fn open() -> Result<Self, CodecError> {
        // SAFETY: loading libva runs only its initializers.
        let va = unsafe { Library::new("libva.so.2") }.map_err(|_| CodecError::NoHardware)?;
        let drm = unsafe { Library::new("libva-drm.so.2") }.map_err(|_| CodecError::NoHardware)?;
        let get_display: unsafe extern "C" fn(c_int) -> Display = symbol!(drm, "vaGetDisplayDRM");
        let initialize: unsafe extern "C" fn(Display, *mut c_int, *mut c_int) -> Status =
            symbol!(va, "vaInitialize");
        let terminate: Fn0 = symbol!(va, "vaTerminate");
        let vendor_string: unsafe extern "C" fn(Display) -> *const c_char =
            symbol!(va, "vaQueryVendorString");
        let max_entrypoints: unsafe extern "C" fn(Display) -> c_int =
            symbol!(va, "vaMaxNumEntrypoints");
        let query_entrypoints: unsafe extern "C" fn(
            Display,
            c_int,
            *mut c_int,
            *mut c_int,
        ) -> Status = symbol!(va, "vaQueryConfigEntrypoints");
        let get_attributes: unsafe extern "C" fn(
            Display,
            c_int,
            c_int,
            *mut ConfigAttrib,
            c_int,
        ) -> Status = symbol!(va, "vaGetConfigAttributes");

        let mut last = CodecError::NoHardware;
        for node in 128..136 {
            let path = format!("/dev/dri/renderD{node}");
            let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
            else {
                continue;
            };
            let node = OwnedFd::from(file);
            let display = unsafe { get_display(node.as_raw_fd()) };
            if display.is_null() {
                continue;
            }
            let (mut major, mut minor) = (0, 0);
            if unsafe { initialize(display, &mut major, &mut minor) } != SUCCESS {
                unsafe { terminate(display) };
                continue;
            }
            let vendor = unsafe { vendor_string(display) };
            let vendor = if vendor.is_null() {
                path.clone()
            } else {
                unsafe { CStr::from_ptr(vendor) }
                    .to_string_lossy()
                    .into_owned()
            };
            let count = unsafe { max_entrypoints(display) }.max(1) as usize;
            let mut entrypoints = vec![0; count];
            let mut found = 0;
            let listed = unsafe {
                query_entrypoints(
                    display,
                    PROFILE_H264_CONSTRAINED_BASELINE,
                    entrypoints.as_mut_ptr(),
                    &mut found,
                )
            } == SUCCESS;
            entrypoints.truncate(if listed { found.max(0) as usize } else { 0 });
            // The full encoder first; the low-power one where it is all.
            let usable = [ENTRYPOINT_ENC_SLICE, ENTRYPOINT_ENC_SLICE_LP]
                .into_iter()
                .filter(|e| entrypoints.contains(e))
                .find(|&entrypoint| {
                    let mut attribs = [
                        ConfigAttrib {
                            kind: ATTRIB_RT_FORMAT,
                            value: 0,
                        },
                        ConfigAttrib {
                            kind: ATTRIB_RATE_CONTROL,
                            value: 0,
                        },
                    ];
                    let asked = unsafe {
                        get_attributes(
                            display,
                            PROFILE_H264_CONSTRAINED_BASELINE,
                            entrypoint,
                            attribs.as_mut_ptr(),
                            attribs.len() as c_int,
                        )
                    } == SUCCESS;
                    let has = |a: &ConfigAttrib, bit: u32| {
                        a.value != ATTRIB_NOT_SUPPORTED && a.value & bit != 0
                    };
                    asked && has(&attribs[0], RT_FORMAT_YUV420) && has(&attribs[1], RC_CBR)
                });
            let Some(entrypoint) = usable else {
                last = hardware(format!("{vendor}: sem H.264 Constrained Baseline"));
                unsafe { terminate(display) };
                continue;
            };
            return Ok(Self {
                display,
                _node: node,
                vendor,
                entrypoint,
                terminate,
                error_str: symbol!(va, "vaErrorStr"),
                create_config: symbol!(va, "vaCreateConfig"),
                destroy_config: symbol!(va, "vaDestroyConfig"),
                create_surfaces: symbol!(va, "vaCreateSurfaces"),
                destroy_surfaces: symbol!(va, "vaDestroySurfaces"),
                create_context: symbol!(va, "vaCreateContext"),
                destroy_context: symbol!(va, "vaDestroyContext"),
                create_buffer: symbol!(va, "vaCreateBuffer"),
                destroy_buffer: symbol!(va, "vaDestroyBuffer"),
                map_buffer: symbol!(va, "vaMapBuffer"),
                unmap_buffer: symbol!(va, "vaUnmapBuffer"),
                create_image: symbol!(va, "vaCreateImage"),
                destroy_image: symbol!(va, "vaDestroyImage"),
                put_image: symbol!(va, "vaPutImage"),
                begin_picture: symbol!(va, "vaBeginPicture"),
                render_picture: symbol!(va, "vaRenderPicture"),
                end_picture: symbol!(va, "vaEndPicture"),
                sync_surface: symbol!(va, "vaSyncSurface"),
                _va: va,
                _drm: drm,
            });
        }
        Err(last)
    }

    /// `status` as a result, naming the call.
    fn check(&self, status: Status, call: &str) -> Result<(), CodecError> {
        if status == SUCCESS {
            return Ok(());
        }
        let text = unsafe { (self.error_str)(status) };
        let text = if text.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        };
        Err(hardware(format!("{call}: {text} ({status})")))
    }

    /// A parameter buffer holding `bytes`.
    fn buffer(&self, context: Id, kind: c_int, bytes: &[u8]) -> Result<Id, CodecError> {
        let mut id = INVALID_ID;
        let status = unsafe {
            (self.create_buffer)(
                self.display,
                context,
                kind,
                bytes.len() as c_uint,
                1,
                bytes.as_ptr().cast_mut().cast(),
                &mut id,
            )
        };
        self.check(status, "vaCreateBuffer")?;
        Ok(id)
    }
}

impl Drop for Va {
    fn drop(&mut self) {
        unsafe { (self.terminate)(self.display) };
    }
}

/// The bytes of a parameter structure, as libva reads them.
fn raw<T: Zeroable>(value: &T) -> &[u8] {
    // SAFETY: a plain C structure; libva reads it whole, padding included
    // (zeroed: every value here starts from `Zeroable::zeroed`).
    unsafe { std::slice::from_raw_parts(ptr::from_ref(value).cast(), size_of::<T>()) }
}

/// A misc parameter buffer: its type, then the parameters.
fn misc<T: Pod>(kind: u32, value: &T) -> Vec<u8> {
    let mut bytes = kind.to_ne_bytes().to_vec();
    bytes.extend_from_slice(bytemuck::bytes_of(value));
    bytes
}

// ---------------------------------------------------------------- encoder

/// What one picture size takes: the configuration, the context, the
/// surfaces and the buffer the output lands in.
struct Session {
    width: u32,
    height: u32,
    config: Id,
    context: Id,
    /// The input, then the two pictures the encoder rebuilds (the current
    /// one and the reference), in turns.
    surfaces: [Id; 3],
    coded: Id,
    image: VaImage,
}

pub struct VaapiEncoder {
    va: Va,
    settings: EncoderSettings,
    session: Option<Session>,
    /// Pictures since the last IDR (0: the next one is an IDR).
    since_idr: u64,
    idr_id: u16,
    /// Which of the two rebuilt pictures is the reference.
    reference: usize,
    /// The bitrate changed: the rate control is told with the next picture.
    rate_changed: bool,
    nv12: Vec<u8>,
    yuv: Option<openh264::formats::YUVBuffer>,
}

impl VaapiEncoder {
    /// Opens the first card that encodes, and checks that the viewer can
    /// decode what it makes.
    pub fn new(settings: EncoderSettings) -> Result<Self, CodecError> {
        let va = Va::open()?;
        let mut encoder = Self {
            va,
            settings,
            session: None,
            since_idr: 0,
            idr_id: 0,
            reference: 0,
            rate_changed: false,
            nv12: Vec::new(),
            yuv: None,
        };
        encoder.probe()?;
        tracing::info!(
            "VA-API: {} ({})",
            encoder.va.vendor,
            if encoder.va.entrypoint == ENTRYPOINT_ENC_SLICE_LP {
                "baixo consumo"
            } else {
                "completo"
            }
        );
        Ok(encoder)
    }

    /// Encodes a few test pictures and decodes them with OpenH264: a
    /// stream the viewer cannot show is an error now, not in a session.
    fn probe(&mut self) -> Result<(), CodecError> {
        let (w, h) = (320, 240);
        let mut decoder = crate::H264Decoder::new()?;
        let mut decoded = 0;
        for t in 0..4u32 {
            let nv12 = probe_image(w, h, t);
            let Some(encoded) = self.encode(&Image::nv12(w, h, &nv12))? else {
                continue;
            };
            if t == 0 {
                let scan = h264::scan(&encoded.data);
                if !scan.has_sps || !scan.has_idr {
                    return Err(hardware("o primeiro quadro não traz SPS e IDR"));
                }
                if scan.profile != Some(h264::PROFILE_BASELINE) {
                    return Err(crate::profile_error(scan.profile.unwrap_or(0)));
                }
            }
            let frame = decoder
                .decode(&encoded.data)
                .map_err(|e| hardware(format!("o visualizador não decodifica: {e}")))?;
            if let Some(frame) = frame {
                let (got, want) = (frame.y[w as usize * 8 + 8], nv12[w as usize * 8 + 8]);
                if (frame.width, frame.height) != (w, h) || got.abs_diff(want) > 24 {
                    return Err(hardware(format!(
                        "imagem decodificada errada: {}x{}, luma {got} (esperado {want})",
                        frame.width, frame.height
                    )));
                }
                decoded += 1;
            }
        }
        if decoded < 3 {
            return Err(hardware(format!("só {decoded} de 4 quadros decodificados")));
        }
        // The session starts over with the first real picture.
        self.close();
        self.since_idr = 0;
        Ok(())
    }

    fn close(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        let va = &self.va;
        let mut surfaces = session.surfaces;
        unsafe {
            (va.destroy_image)(va.display, session.image.image_id);
            (va.destroy_buffer)(va.display, session.coded);
            (va.destroy_context)(va.display, session.context);
            (va.destroy_surfaces)(va.display, surfaces.as_mut_ptr(), 3);
            (va.destroy_config)(va.display, session.config);
        }
    }

    /// The session for pictures of `width` x `height`.
    fn open_session(&mut self, width: u32, height: u32) -> Result<(), CodecError> {
        self.close();
        let va = &self.va;
        let ((mw, mh), _) = macroblocks(width, height);
        let (aw, ah) = (u32::from(mw) * 16, u32::from(mh) * 16);
        let mut attribs = [
            ConfigAttrib {
                kind: ATTRIB_RT_FORMAT,
                value: RT_FORMAT_YUV420,
            },
            ConfigAttrib {
                kind: ATTRIB_RATE_CONTROL,
                value: RC_CBR,
            },
        ];
        let mut config = INVALID_ID;
        va.check(
            unsafe {
                (va.create_config)(
                    va.display,
                    PROFILE_H264_CONSTRAINED_BASELINE,
                    va.entrypoint,
                    attribs.as_mut_ptr(),
                    attribs.len() as c_int,
                    &mut config,
                )
            },
            "vaCreateConfig",
        )?;
        let mut surfaces = [INVALID_ID; 3];
        let status = unsafe {
            (va.create_surfaces)(
                va.display,
                RT_FORMAT_YUV420,
                aw,
                ah,
                surfaces.as_mut_ptr(),
                3,
                ptr::null_mut(),
                0,
            )
        };
        if let Err(e) = va.check(status, "vaCreateSurfaces") {
            unsafe { (va.destroy_config)(va.display, config) };
            return Err(e);
        }
        let mut context = INVALID_ID;
        let status = unsafe {
            (va.create_context)(
                va.display,
                config,
                aw as c_int,
                ah as c_int,
                PROGRESSIVE,
                surfaces.as_mut_ptr(),
                3,
                &mut context,
            )
        };
        let mut coded = INVALID_ID;
        let mut image = VaImage::zeroed();
        image.image_id = INVALID_ID;
        let made = va.check(status, "vaCreateContext").and_then(|()| {
            // Room for a picture with no compression at all.
            let size = (aw * ah * 3 / 2 + 64 * 1024) as c_uint;
            va.check(
                unsafe {
                    (va.create_buffer)(
                        va.display,
                        context,
                        BUFFER_CODED,
                        size,
                        1,
                        ptr::null_mut(),
                        &mut coded,
                    )
                },
                "vaCreateBuffer",
            )?;
            let mut format = ImageFormat {
                fourcc: FOURCC_NV12,
                byte_order: LSB_FIRST,
                bits_per_pixel: 12,
                ..ImageFormat::zeroed()
            };
            va.check(
                unsafe {
                    (va.create_image)(
                        va.display,
                        &mut format,
                        aw as c_int,
                        ah as c_int,
                        &mut image,
                    )
                },
                "vaCreateImage",
            )
        });
        let session = Session {
            width,
            height,
            config,
            context,
            surfaces,
            coded,
            image,
        };
        // (Whatever was made is undone by `close`.)
        self.session = Some(session);
        made.inspect_err(|_| self.close())?;
        self.since_idr = 0;
        self.reference = 0;
        Ok(())
    }

    /// Copies NV12 `data` (`width` x `height`) into the input surface,
    /// repeating the last row and column out to the macroblock edge.
    fn upload(&self, session: &Session, data: &[u8]) -> Result<(), CodecError> {
        let va = &self.va;
        let image = &session.image;
        let (w, h) = (session.width as usize, session.height as usize);
        let (aw, ah) = (usize::from(image.width), usize::from(image.height));
        let mut mapped: *mut c_void = ptr::null_mut();
        va.check(
            unsafe { (va.map_buffer)(va.display, image.buf, &mut mapped) },
            "vaMapBuffer",
        )?;
        let size = image.data_size as usize;
        // SAFETY: the image's buffer, mapped, `data_size` bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(mapped.cast::<u8>(), size) };
        let planes = [
            (
                image.offsets[0] as usize,
                image.pitches[0] as usize,
                0,
                h,
                ah,
            ),
            (
                image.offsets[1] as usize,
                image.pitches[1] as usize,
                w * h,
                h / 2,
                ah / 2,
            ),
        ];
        let mut fits = true;
        for (offset, pitch, start, rows, all_rows) in planes {
            if pitch < aw || offset + pitch * all_rows > size {
                fits = false;
                break;
            }
            for row in 0..all_rows {
                let source = start + row.min(rows - 1) * w;
                let line = &data[source..source + w];
                let target = &mut out[offset + row * pitch..offset + row * pitch + aw];
                target[..w].copy_from_slice(line);
                // Past the right edge: the last pixel again (a U/V pair in
                // the chroma plane, which `w` keeps even).
                for x in (w..aw).step_by(2) {
                    target[x] = line[w - 2];
                    target[x + 1] = line[w - 1];
                }
            }
        }
        unsafe { (va.unmap_buffer)(va.display, image.buf) };
        if !fits {
            return Err(hardware("imagem NV12 do driver com outro formato"));
        }
        va.check(
            unsafe {
                (va.put_image)(
                    va.display,
                    session.surfaces[0],
                    image.image_id,
                    0,
                    0,
                    aw as c_uint,
                    ah as c_uint,
                    0,
                    0,
                    aw as c_uint,
                    ah as c_uint,
                )
            },
            "vaPutImage",
        )
    }

    /// The parameters of the next picture, as buffers.
    fn parameters(&self, session: &Session, idr: bool) -> Result<Vec<Id>, CodecError> {
        let va = &self.va;
        let ((mw, mh), (crop_right, crop_bottom)) = macroblocks(session.width, session.height);
        let bitrate = self.settings.bitrate_bps.max(100_000);
        let fps = (self.settings.max_fps.round() as u32).clamp(1, 240);
        let mut buffers = Vec::new();
        if idr || self.rate_changed {
            let mut seq = SequenceH264::zeroed();
            seq.level_idc = 51;
            seq.intra_period = IDR_EVERY as u32;
            seq.intra_idr_period = IDR_EVERY as u32;
            seq.ip_period = 1;
            seq.bits_per_second = bitrate;
            seq.max_num_ref_frames = 1;
            seq.picture_width_in_mbs = mw;
            seq.picture_height_in_mbs = mh;
            seq.seq_fields = seq_fields();
            if crop_right > 0 || crop_bottom > 0 {
                seq.frame_cropping_flag = 1;
                seq.frame_crop_right_offset = crop_right;
                seq.frame_crop_bottom_offset = crop_bottom;
            }
            buffers.push(va.buffer(session.context, BUFFER_SEQUENCE, raw(&seq))?);
            let rate = MiscRateControl {
                bits_per_second: bitrate,
                target_percentage: 100,
                window_size: 1000,
                initial_qp: 26,
                min_qp: 10,
                // Never skips a picture: a skipped one would be a lost update.
                rc_flags: 1 << 1,
                ..MiscRateControl::zeroed()
            };
            buffers.push(va.buffer(
                session.context,
                BUFFER_MISC,
                &misc(MISC_RATE_CONTROL, &rate),
            )?);
            let frame_rate = MiscFrameRate {
                framerate: fps | (1 << 16),
                ..MiscFrameRate::zeroed()
            };
            buffers.push(va.buffer(
                session.context,
                BUFFER_MISC,
                &misc(MISC_FRAME_RATE, &frame_rate),
            )?);
            let hrd = MiscHrd {
                buffer_size: bitrate,
                initial_buffer_fullness: bitrate / 2,
                ..MiscHrd::zeroed()
            };
            buffers.push(va.buffer(session.context, BUFFER_MISC, &misc(MISC_HRD, &hrd))?);
        }

        let order = (self.since_idr * 2) as u32;
        let current = PictureH264 {
            picture_id: session.surfaces[1 + (1 - self.reference)],
            frame_idx: self.since_idr as u32,
            flags: 0,
            top_field_order_cnt: order as i32,
            bottom_field_order_cnt: 0,
            reserved: [0; 4],
        };
        let reference = PictureH264 {
            picture_id: session.surfaces[1 + self.reference],
            frame_idx: (self.since_idr.saturating_sub(1)) as u32,
            flags: PICTURE_SHORT_TERM_REFERENCE,
            top_field_order_cnt: order.saturating_sub(2) as i32,
            bottom_field_order_cnt: 0,
            reserved: [0; 4],
        };
        let mut pic = PictureParamH264::zeroed();
        pic.curr_pic = current;
        pic.reference_frames = [PictureH264::INVALID; 16];
        if !idr {
            pic.reference_frames[0] = reference;
        }
        pic.coded_buf = session.coded;
        pic.frame_num = (self.since_idr % (1 << LOG2_MAX)) as u16;
        pic.pic_init_qp = 26;
        pic.pic_fields = pic_fields(idr);
        buffers.push(va.buffer(session.context, BUFFER_PICTURE, raw(&pic))?);

        let mut slice = SliceParamH264::zeroed();
        slice.num_macroblocks = u32::from(mw) * u32::from(mh);
        slice.macroblock_info = INVALID_ID;
        slice.slice_type = if idr { SLICE_I } else { SLICE_P };
        slice.idr_pic_id = self.idr_id;
        slice.pic_order_cnt_lsb = (order % (1 << LOG2_MAX)) as u16;
        slice.ref_pic_list0 = [PictureH264::INVALID; 32];
        slice.ref_pic_list1 = [PictureH264::INVALID; 32];
        if !idr {
            slice.ref_pic_list0[0] = reference;
        }
        buffers.push(va.buffer(session.context, BUFFER_SLICE, raw(&slice))?);
        Ok(buffers)
    }

    /// Encodes the picture in the input surface; the output, from the coded
    /// buffer.
    fn run(&self, idr: bool) -> Result<Vec<u8>, CodecError> {
        let session = self.session.as_ref().expect("session opened");
        let va = &self.va;
        let buffers = self.parameters(session, idr)?;
        let encoded = (|| {
            va.check(
                unsafe { (va.begin_picture)(va.display, session.context, session.surfaces[0]) },
                "vaBeginPicture",
            )?;
            let mut list = buffers.clone();
            va.check(
                unsafe {
                    (va.render_picture)(
                        va.display,
                        session.context,
                        list.as_mut_ptr(),
                        list.len() as c_int,
                    )
                },
                "vaRenderPicture",
            )?;
            va.check(
                unsafe { (va.end_picture)(va.display, session.context) },
                "vaEndPicture",
            )?;
            va.check(
                unsafe { (va.sync_surface)(va.display, session.surfaces[0]) },
                "vaSyncSurface",
            )?;
            let mut mapped: *mut c_void = ptr::null_mut();
            va.check(
                unsafe { (va.map_buffer)(va.display, session.coded, &mut mapped) },
                "vaMapBuffer",
            )?;
            let mut out = Vec::new();
            let mut segment = mapped.cast::<CodedSegment>();
            while !segment.is_null() {
                // SAFETY: the driver's list of output pieces, valid while
                // the coded buffer is mapped.
                let piece = unsafe { &*segment };
                if !piece.buf.is_null() && piece.size > 0 {
                    out.extend_from_slice(unsafe {
                        std::slice::from_raw_parts(piece.buf.cast::<u8>(), piece.size as usize)
                    });
                }
                segment = piece.next.cast();
            }
            unsafe { (va.unmap_buffer)(va.display, session.coded) };
            Ok(out)
        })();
        for buffer in buffers {
            unsafe { (va.destroy_buffer)(va.display, buffer) };
        }
        encoded
    }
}

/// A test picture in NV12: a gradient with a block that moves with `t`.
fn probe_image(w: u32, h: u32, t: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    for y in 0..h {
        for x in 0..w {
            let block = (x / 16 + t as usize).is_multiple_of(4) && y < h / 2;
            out.push(if block { 200 } else { 40 + (y * 100 / h) as u8 });
        }
    }
    out.resize(w * h * 3 / 2, 128);
    out
}

impl VideoEncoder for VaapiEncoder {
    fn encode(&mut self, image: &Image<'_>) -> Result<Option<Encoded>, CodecError> {
        check_frame(image)?;
        let (width, height) = (image.width, image.height);
        let data = image
            .memory()
            .ok_or_else(|| hardware("imagem fora da memória"))?;
        if !matches!(&self.session, Some(s) if (s.width, s.height) == (width, height)) {
            self.open_session(width, height)?;
        }
        if image.format == PixelFormat::Bgra {
            crate::bgra_to_nv12(width, height, data, &mut self.yuv, &mut self.nv12);
        }
        if self.since_idr >= IDR_EVERY {
            self.since_idr = 0;
        }
        let idr = self.since_idr == 0;
        let nv12 = match image.format {
            PixelFormat::Nv12 => data,
            PixelFormat::Bgra => &self.nv12,
        };
        let session = self.session.as_ref().expect("session opened");
        self.upload(session, nv12)?;
        let data = self.run(idr)?;
        self.rate_changed = false;
        if idr {
            self.idr_id = self.idr_id.wrapping_add(1);
        }
        self.since_idr += 1;
        // What was rebuilt is the next one's reference.
        self.reference = 1 - self.reference;
        if data.is_empty() {
            return Ok(None);
        }
        Ok(Some(Encoded {
            keyframe: h264::has_idr(&data),
            data,
        }))
    }

    fn force_keyframe(&mut self) {
        self.since_idr = 0;
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) {
        if bitrate_bps != self.settings.bitrate_bps {
            self.settings.bitrate_bps = bitrate_bps;
            self.rate_changed = true;
        }
    }

    fn name(&self) -> String {
        format!("GPU (VA-API: {})", self.va.vendor)
    }

    fn is_hardware(&self) -> bool {
        true
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_fields() {
        // chroma 1, frames only, 8x8 inference, 12 and 12 in the two log2 fields.
        assert_eq!(seq_fields(), 1 | 1 << 2 | 1 << 5 | 12 << 6 | 12 << 12);
        assert_eq!(pic_fields(true), 1 | 1 << 1 | 1 << 9);
        assert_eq!(pic_fields(false), 1 << 1 | 1 << 9);
    }

    #[test]
    fn sizes_round_up_to_macroblocks() {
        assert_eq!(macroblocks(1920, 1080), ((120, 68), (0, 4)));
        assert_eq!(macroblocks(1366, 768), ((86, 48), (5, 0)));
        assert_eq!(macroblocks(320, 240), ((20, 15), (0, 0)));
    }

    /// The structures here against libva's headers: `va-layout` (built in CI
    /// from `va-layout.c`, against libva-dev) prints each size and offset,
    /// and they must all agree.
    #[test]
    #[ignore = "needs libva's layouts: HEXT_VA_LAYOUT=va-layout.txt cargo test -- --ignored layouts"]
    fn layouts_match_libva() {
        use std::mem::offset_of;
        let path = std::env::var("HEXT_VA_LAYOUT").expect("HEXT_VA_LAYOUT");
        let text = std::fs::read_to_string(path).unwrap();
        let ours: &[(&str, usize)] = &[
            ("VAConfigAttrib", size_of::<ConfigAttrib>()),
            ("VAPictureH264", size_of::<PictureH264>()),
            (
                "VAEncSequenceParameterBufferH264",
                size_of::<SequenceH264>(),
            ),
            (
                "VAEncSequenceParameterBufferH264.seq_fields",
                offset_of!(SequenceH264, seq_fields),
            ),
            (
                "VAEncSequenceParameterBufferH264.frame_cropping_flag",
                offset_of!(SequenceH264, frame_cropping_flag),
            ),
            (
                "VAEncSequenceParameterBufferH264.time_scale",
                offset_of!(SequenceH264, time_scale),
            ),
            (
                "VAEncPictureParameterBufferH264",
                size_of::<PictureParamH264>(),
            ),
            (
                "VAEncPictureParameterBufferH264.coded_buf",
                offset_of!(PictureParamH264, coded_buf),
            ),
            (
                "VAEncPictureParameterBufferH264.frame_num",
                offset_of!(PictureParamH264, frame_num),
            ),
            (
                "VAEncPictureParameterBufferH264.pic_fields",
                offset_of!(PictureParamH264, pic_fields),
            ),
            ("VAEncSliceParameterBufferH264", size_of::<SliceParamH264>()),
            (
                "VAEncSliceParameterBufferH264.RefPicList0",
                offset_of!(SliceParamH264, ref_pic_list0),
            ),
            (
                "VAEncSliceParameterBufferH264.luma_weight_l0",
                offset_of!(SliceParamH264, luma_weight_l0),
            ),
            (
                "VAEncSliceParameterBufferH264.chroma_weight_l1",
                offset_of!(SliceParamH264, chroma_weight_l1),
            ),
            (
                "VAEncSliceParameterBufferH264.cabac_init_idc",
                offset_of!(SliceParamH264, cabac_init_idc),
            ),
            (
                "VAEncMiscParameterRateControl",
                size_of::<MiscRateControl>(),
            ),
            ("VAEncMiscParameterFrameRate", size_of::<MiscFrameRate>()),
            ("VAEncMiscParameterHRD", size_of::<MiscHrd>()),
            ("VAImageFormat", size_of::<ImageFormat>()),
            ("VAImage", size_of::<VaImage>()),
            ("VAImage.buf", offset_of!(VaImage, buf)),
            ("VAImage.pitches", offset_of!(VaImage, pitches)),
            ("VAImage.offsets", offset_of!(VaImage, offsets)),
            ("VACodedBufferSegment", size_of::<CodedSegment>()),
            ("VACodedBufferSegment.buf", offset_of!(CodedSegment, buf)),
            ("VACodedBufferSegment.next", offset_of!(CodedSegment, next)),
        ];
        let theirs: std::collections::HashMap<&str, usize> = text
            .lines()
            .filter_map(|line| {
                let (name, value) = line.split_once(' ')?;
                Some((name, value.trim().parse().ok()?))
            })
            .collect();
        for (name, value) in ours {
            assert_eq!(theirs.get(name), Some(value), "{name}");
        }
        assert_eq!(theirs.len(), ours.len(), "names in va-layout.c and here");
    }

    /// On a machine with a card that encodes (by hand: CI has none): the
    /// encoder opens, passes its own check and keeps a stream going.
    #[test]
    #[ignore = "needs a graphics card with VA-API"]
    fn vaapi_encodes_on_this_machine() {
        let mut encoder = VaapiEncoder::new(EncoderSettings::default()).unwrap();
        eprintln!("{}", encoder.name());
        let mut decoder = crate::H264Decoder::new().unwrap();
        for t in 0..30 {
            let image = crate::tests::nv12_image(1366, 768, t);
            let encoded = encoder.encode(&Image::nv12(1366, 768, &image)).unwrap();
            if let Some(encoded) = encoded {
                assert_eq!(encoded.keyframe, t == 0);
                decoder.decode(&encoded.data).unwrap();
            }
        }
    }
}
