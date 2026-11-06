//! Hardware H.264 encoding through Media Foundation.
//!
//! GPU vendors ship their encoders as asynchronous Media Foundation
//! transforms (MFTs). The transform asks for input (`METransformNeedInput`)
//! and announces output (`METransformHaveOutput`) through events. Frames go
//! in as NV12: straight from the capture's GPU texture when the transform
//! runs on the capture's adapter and supports Direct3D 11 (zero-copy: the
//! image never leaves the GPU), otherwise from system memory.
//!
//! The decoder on the other side only handles the Baseline profile, so the
//! encoder is asked for it and every keyframe's SPS is checked: an encoder
//! that ignores the request fails here and `Auto` mode falls back to software.

// COM / Media Foundation calls are unsafe by nature; each block below is a plain API call.
#![allow(unsafe_code)]

use std::mem::ManuallyDrop;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use openh264::formats::YUVBuffer;
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonBufferSize, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
    ICodecAPI, IMFActivate, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFShutdown,
    IMFTransform, METransformHaveOutput, METransformNeedInput, MF_16_BYTE_ALIGNMENT,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
    MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
    MFCreateAlignedMemoryBuffer, MFCreateMediaType, MFCreateSample, MFMediaType_Video,
    MFSTARTUP_FULL, MFSampleExtension_CleanPoint, MFShutdown, MFStartup,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_Base,
};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFAttributes,
    IMFDXGIDeviceManager, METransformDrainComplete, MF_SA_D3D11_AWARE, MFCreateAttributes,
    MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFT_ENUM_ADAPTER_LUID,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_SET_D3D_MANAGER, MFTEnum2,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4,
};
use windows::core::{GUID, Interface, PWSTR, Ref, implement};

use crate::d3d11::Readback;
use crate::h264::{self, PROFILE_BASELINE};
use crate::nv12::bgra_to_nv12;
use crate::{
    CodecError, Encoded, EncoderSettings, Image, PixelFormat, Pixels, VideoEncoder, check_frame,
    merge, profile_error,
};

/// Most GPU-texture frames the transform may hold at once. The capture
/// reuses a ring of textures (6), so a texture must be encoded before the
/// capture comes back around to it.
const MAX_TEXTURES_IN_FLIGHT: u32 = 3;

/// Longest wait for the encoder to ask for input or deliver output.
const EVENT_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a still screen waits for frames left in the encoder before
/// asking it to flush them.
const DRAIN_WAIT: Duration = Duration::from_millis(30);

/// Hundreds of nanoseconds per second (Media Foundation time unit).
pub(crate) const HNS_PER_SEC: i64 = 10_000_000;

/// A keyframe at least every minute, so a lost one can never stall the
/// picture for long even if the viewer's request goes missing.
const GOP_FRAMES: u32 = 60 * 60;

pub(crate) fn hw(context: &str, e: windows::core::Error) -> CodecError {
    CodecError::Hardware(format!("{context}: {e}"))
}

pub struct MfEncoder {
    settings: EncoderSettings,
    name: String,
    /// Recreated whenever the input size changes.
    session: Option<Session>,
    force_keyframe: bool,
    frame_index: i64,
    yuv: Option<YUVBuffer>,
    nv12: Vec<u8>,
    /// GPU textures the transform cannot take, read back into `nv12`.
    readback: Readback,
    /// Encoders that failed taking textures directly: fed through memory.
    zero_copy_failed: Vec<String>,
    /// The last try gave the encoder textures (for `degrade`).
    tried_gpu_input: bool,
    /// Encoders that failed even through memory: the next ones are tried.
    failed: Vec<String>,
    /// Which settings to give the encoder (all, normally; fewer when
    /// probing what a driver accepts).
    tweaks: Tweaks,
    /// Only this encoder (probing).
    only: Option<String>,
    /// Memory images go with this device attached (probing).
    memory_device: Option<ID3D11Device>,
}

/// The optional encoder settings, each of which a driver may refuse.
#[derive(Debug, Clone, Copy)]
pub struct Tweaks {
    /// `MF_LOW_LATENCY` and `CODECAPI_AVLowLatencyMode`.
    pub low_latency: bool,
    /// Constant bit rate at the configured rate.
    pub rate_control: bool,
    /// A long GOP and no B frames.
    pub gop: bool,
    /// Baseline profile asked for in the output type.
    pub profile: bool,
    /// Media types taken from those the transform offers, then filled in,
    /// instead of made from scratch.
    pub offered_types: bool,
    /// Our own output sample, even when the transform says it provides one.
    pub own_output_sample: bool,
    /// `MFT_MESSAGE_NOTIFY_START_OF_STREAM` after `BEGIN_STREAMING`.
    pub start_message: bool,
}

impl Tweaks {
    pub const ALL: Self = Self {
        low_latency: true,
        rate_control: true,
        gop: true,
        profile: true,
        offered_types: false,
        own_output_sample: false,
        start_message: true,
    };
}

/// How long to wait, after an output format change, for the transform to
/// announce the output again.
const AFTER_FORMAT_CHANGE: Duration = Duration::from_millis(100);

/// How many times one image is retried on another way to encode it
/// (without zero-copy, then another encoder) before giving up on the GPU.
const MAX_ATTEMPTS: usize = 4;

struct Session {
    /// The transform's stream numbers.
    input_id: u32,
    output_id: u32,
    width: u32,
    height: u32,
    /// The capture's D3D11 device this session was opened for, if any.
    device: Option<ID3D11Device>,
    /// The transform takes textures of `device` directly (zero-copy).
    gpu_input: bool,
    /// Keeps the device manager handed to the transform alive.
    _manager: Option<IMFDXGIDeviceManager>,
    activate: IMFActivate,
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    /// Receives the type of each event, delivered by [`EventSink`].
    event_rx: mpsc::Receiver<windows::core::Result<(u32, windows::core::HRESULT)>>,
    callback: IMFAsyncCallback,
    codec: Option<ICodecAPI>,
    /// `METransformNeedInput` events not yet answered with an input.
    need_input: u32,
    /// The transform allocates output samples itself.
    provides_samples: bool,
    /// Gives the transform output samples even if it would provide them.
    own_output_sample: bool,
    output_size: u32,
    /// Inputs given to the transform whose output has not come back yet.
    in_flight: u32,
    /// A drain was requested and `METransformDrainComplete` has not arrived.
    draining: bool,
    /// SPS/PPS from the output type, for encoders that do not repeat them inline.
    sequence_header: Vec<u8>,
}

impl MfEncoder {
    pub fn new(settings: EncoderSettings) -> Result<Self, CodecError> {
        unsafe {
            // Media Foundation needs COM on this (the capture) thread.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| hw("MFStartup", e))?;
        }
        let name = match enumerate(None) {
            Ok(encoders) => encoders.first().map(friendly_name),
            Err(e) => {
                unsafe {
                    let _ = MFShutdown();
                }
                return Err(e);
            }
        };
        let Some(name) = name else {
            unsafe {
                let _ = MFShutdown();
            }
            return Err(CodecError::NoHardware);
        };
        tracing::info!("codificador por hardware: {name}");
        Ok(Self {
            settings,
            name,
            session: None,
            force_keyframe: false,
            frame_index: 0,
            yuv: None,
            nv12: Vec::new(),
            readback: Readback::default(),
            zero_copy_failed: Vec::new(),
            tried_gpu_input: false,
            failed: Vec::new(),
            tweaks: Tweaks::ALL,
            only: None,
            memory_device: None,
        })
    }

    /// After a failure with the current session, the next way to try:
    /// the same encoder through memory, then the other encoders (opening
    /// the session finds there is none left, with `NoHardware`).
    fn degrade(&mut self, gpu_input: bool, error: &CodecError) {
        // The next try opens a session the new way.
        self.session = None;
        if gpu_input && !self.zero_copy_failed.contains(&self.name) {
            tracing::warn!(
                "{}: zero-copy falhou ({error}); imagem pela memória",
                self.name
            );
            self.zero_copy_failed.push(self.name.clone());
        } else if !self.failed.contains(&self.name) {
            tracing::warn!(
                "{} falhou ({error}); tentando outro codificador da GPU",
                self.name
            );
            self.failed.push(self.name.clone());
        }
    }

    /// Replaces the session (the new one starts with a keyframe).
    fn reopen(
        &mut self,
        width: u32,
        height: u32,
        device: Option<&ID3D11Device>,
    ) -> Result<(), CodecError> {
        self.session = None;
        let session = self.open_session(width, height, device)?;
        self.session = Some(session);
        self.frame_index = 0;
        Ok(())
    }

    /// Opens an encoder for `width`x`height` NV12 images. With `device` (the
    /// capture's), an encoder on the same adapter is preferred, and is given
    /// the device so it can read the capture's textures directly.
    fn open_session(
        &mut self,
        width: u32,
        height: u32,
        device: Option<&ID3D11Device>,
    ) -> Result<Session, CodecError> {
        // An encoder on the capture's adapter, or else any (fed from
        // memory); none that already failed.
        let usable = |list: Vec<IMFActivate>| {
            list.into_iter().find(|a| {
                let name = friendly_name(a);
                !self.failed.contains(&name) && self.only.as_ref().is_none_or(|only| *only == name)
            })
        };
        let on_adapter = device.and_then(|device| {
            enumerate(Some(device))
                .inspect_err(|e| tracing::debug!("encoders do adaptador da captura: {e}"))
                .ok()
                .and_then(usable)
        });
        let same_adapter = on_adapter.is_some();
        let activate = match on_adapter {
            Some(activate) => activate,
            None => usable(enumerate(None)?).ok_or(CodecError::NoHardware)?,
        };
        // Named before it can fail, so a failure counts against this one.
        let name = friendly_name(&activate);
        if name != self.name {
            tracing::info!("codificador por hardware: {name}");
            self.name = name;
        }
        let transform: IMFTransform =
            unsafe { activate.ActivateObject() }.map_err(|e| hw("ActivateObject", e))?;

        let fps = self.settings.max_fps.round().max(1.0) as u32;

        // An async transform refuses every message until unlocked.
        unsafe {
            let attributes = transform
                .GetAttributes()
                .map_err(|e| hw("GetAttributes", e))?;
            attributes
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .map_err(|e| hw("async unlock", e))?;
            if self.tweaks.low_latency {
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            }
        }

        // (Asked after the unlock, which every call before it fails.)
        // Most transforms number their streams from 0 (and do not say so,
        // E_NOTIMPL); some do not.
        let (mut input_ids, mut output_ids) = ([0u32], [0u32]);
        if unsafe { transform.GetStreamIDs(&mut input_ids, &mut output_ids) }.is_err() {
            (input_ids, output_ids) = ([0], [0]);
        }
        let (input_id, output_id) = (input_ids[0], output_ids[0]);

        // The device goes in before the media types.
        let manager = match device {
            Some(device) if same_adapter && !self.zero_copy_failed.contains(&self.name) => {
                attach_device(&transform, device)
                    .inspect_err(|e| {
                        tracing::info!(
                            "o encoder não lê texturas da GPU ({e}); imagem pela memória"
                        );
                    })
                    .ok()
            }
            _ => None,
        };
        // A failure from here on, with textures, may be the zero-copy's.
        self.tried_gpu_input = manager.is_some();
        if manager.is_some() {
            tracing::info!("zero-copy: o encoder lê a imagem direto da GPU");
        } else if device.is_some() && !same_adapter {
            tracing::info!("encoder em outro adaptador que a captura; imagem pela memória");
        } else if device.is_some() {
            tracing::info!("imagem pela memória (o zero-copy falhou antes)");
        }

        // Encoder settings are best effort: drivers support different subsets.
        let codec: Option<ICodecAPI> = transform.cast().ok();
        if let Some(codec) = &codec {
            let tweaks = self.tweaks;
            if tweaks.low_latency {
                set(codec, &CODECAPI_AVLowLatencyMode, bool_variant());
            }
            if tweaks.rate_control {
                set(
                    codec,
                    &CODECAPI_AVEncCommonRateControlMode,
                    u32_variant(eAVEncCommonRateControlMode_CBR.0 as u32),
                );
                set(
                    codec,
                    &CODECAPI_AVEncCommonMeanBitRate,
                    u32_variant(self.settings.bitrate_bps),
                );
                // Temporary, to find a value in the field: a short buffer
                // keeps a full screen change from becoming a huge frame, at
                // the cost of a moment of lower quality.
                let buffer_ms = std::env::var("HEXT_VBV_MS").ok();
                if let Some(ms) = buffer_ms.and_then(|ms| ms.parse::<u64>().ok()) {
                    let bits = u64::from(self.settings.bitrate_bps) * ms / 1000;
                    let bits = u32::try_from(bits).unwrap_or(u32::MAX);
                    let taken = set(codec, &CODECAPI_AVEncCommonBufferSize, u32_variant(bits));
                    tracing::info!("buffer do codificador: {ms} ms, aceito: {taken}");
                }
            }
            if tweaks.gop {
                set(codec, &CODECAPI_AVEncMPVGOPSize, u32_variant(GOP_FRAMES));
                set(
                    codec,
                    &CODECAPI_AVEncMPVDefaultBPictureCount,
                    u32_variant(0),
                );
            }
        }

        // Encoders want the output type before the input type.
        unsafe {
            let output = if self.tweaks.offered_types {
                offered_type(&transform, output_id, true, &MFVideoFormat_H264)?
            } else {
                MFCreateMediaType().map_err(|e| hw("MFCreateMediaType", e))?
            };
            fill_video_type(&output, &MFVideoFormat_H264, width, height, fps)?;
            output
                .SetUINT32(&MF_MT_AVG_BITRATE, self.settings.bitrate_bps)
                .map_err(|e| hw("bitrate", e))?;
            if self.tweaks.profile {
                output
                    .SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)
                    .map_err(|e| hw("profile", e))?;
            }
            transform
                .SetOutputType(output_id, &output, 0)
                .map_err(|e| hw("SetOutputType", e))?;

            let input = if self.tweaks.offered_types {
                offered_type(&transform, input_id, false, &MFVideoFormat_NV12)?
            } else {
                MFCreateMediaType().map_err(|e| hw("MFCreateMediaType", e))?
            };
            fill_video_type(&input, &MFVideoFormat_NV12, width, height, fps)?;
            input
                .SetUINT32(&MF_MT_DEFAULT_STRIDE, width)
                .map_err(|e| hw("stride", e))?;
            transform
                .SetInputType(input_id, &input, 0)
                .map_err(|e| hw("SetInputType (NV12)", e))?;

            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| hw("begin streaming", e))?;
            if self.tweaks.start_message {
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .map_err(|e| hw("start of stream", e))?;
            }
        }

        let info = unsafe { transform.GetOutputStreamInfo(output_id) }
            .map_err(|e| hw("stream info", e))?;
        tracing::info!(
            "streams: entrada {input_id}, saída {output_id} (flags {:#x}, {} bytes)",
            info.dwFlags,
            info.cbSize
        );
        let provides =
            (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
        let events: IMFMediaEventGenerator =
            transform.cast().map_err(|e| hw("event generator", e))?;
        let (event_tx, event_rx) = mpsc::channel();
        let callback: IMFAsyncCallback = EventSink {
            generator: events.clone(),
            tx: event_tx,
        }
        .into();
        let mut session = Session {
            input_id,
            output_id,
            width,
            height,
            device: device.cloned(),
            gpu_input: manager.is_some(),
            _manager: manager,
            activate,
            transform,
            events,
            event_rx,
            callback,
            codec,
            need_input: 0,
            provides_samples: info.dwFlags & provides != 0 && !self.tweaks.own_output_sample,
            own_output_sample: self.tweaks.own_output_sample,
            output_size: info.cbSize.max(width * height * 3 / 2),
            in_flight: 0,
            draining: false,
            sequence_header: Vec::new(),
        };
        session.refresh_sequence_header();
        session.request_event()?;
        Ok(session)
    }
}

impl VideoEncoder for MfEncoder {
    fn encode(&mut self, image: &Image<'_>) -> Result<Option<Encoded>, CodecError> {
        check_frame(image)?;
        // The capture's device (not the encoder's fault if this fails).
        let device = match image.pixels {
            Pixels::D3d11(texture) => {
                Some(unsafe { texture.GetDevice() }.map_err(|e| hw("GetDevice", e))?)
            }
            Pixels::Memory(_) => self.memory_device.clone(),
        };
        let mut first_error = None;
        for _ in 0..MAX_ATTEMPTS {
            self.tried_gpu_input = self.session.as_ref().is_some_and(|s| s.gpu_input);
            match self.encode_once(image, device.as_ref()) {
                Ok(encoded) => return Ok(encoded),
                // No encoder left to try: the first failure says why.
                Err(CodecError::NoHardware) if first_error.is_some() => break,
                Err(e) => {
                    self.degrade(self.tried_gpu_input, &e);
                    first_error.get_or_insert(e);
                }
            }
        }
        Err(first_error.unwrap_or(CodecError::NoHardware))
    }

    fn pending(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.in_flight > 0)
    }

    fn drain(&mut self) -> Result<Option<Encoded>, CodecError> {
        let Some(session) = &mut self.session else {
            return Ok(None);
        };
        let result = session.drain();
        if result.is_err() {
            self.session = None;
        }
        result
    }

    fn force_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn name(&self) -> String {
        format!("GPU ({})", self.name)
    }

    fn is_hardware(&self) -> bool {
        true
    }
}

impl MfEncoder {
    /// One try at encoding `image` with the current way (opening it if
    /// needed).
    fn encode_once(
        &mut self,
        image: &Image<'_>,
        device: Option<&ID3D11Device>,
    ) -> Result<Option<Encoded>, CodecError> {
        let (width, height) = (image.width, image.height);
        let device = device.cloned();
        // A new size, the first texture (or a new capture device), or memory
        // images after textures: the encoder must be opened for it.
        let reopen = match &self.session {
            Some(s) => {
                (s.width, s.height) != (width, height)
                    || (device.is_some() && s.device != device)
                    || (device.is_none() && s.gpu_input)
            }
            None => true,
        };
        if reopen {
            self.reopen(width, height, device.as_ref())?;
        }

        // A session just opened starts with a keyframe anyway.
        if std::mem::take(&mut self.force_keyframe) && !reopen {
            let session = self.session.as_ref().expect("session opened above");
            let forced = session
                .codec
                .as_ref()
                .is_some_and(|codec| set(codec, &CODECAPI_AVEncVideoForceKeyFrame, u32_variant(1)));
            if !forced {
                // The driver cannot force one; a new session starts with a keyframe.
                self.reopen(width, height, device.as_ref())?;
            }
        }

        // Built for the session that will take it (after any reopen above).
        let gpu_input = self.session.as_ref().is_some_and(|s| s.gpu_input);
        self.tried_gpu_input = gpu_input;
        let fps = i64::from(self.settings.max_fps.round().max(1.0) as u32);
        let duration = HNS_PER_SEC / fps;
        let time = self.frame_index * duration;
        let sample = match (image.pixels, image.format) {
            (Pixels::D3d11(texture), _) if gpu_input => texture_sample(texture, time, duration)?,
            (Pixels::D3d11(texture), _) => {
                self.readback.read(texture, width, height, &mut self.nv12)?;
                memory_sample(&self.nv12, time, duration)?
            }
            (Pixels::Memory(data), PixelFormat::Nv12) => memory_sample(data, time, duration)?,
            // The transform takes NV12; BGRA is converted on the CPU first.
            (Pixels::Memory(data), PixelFormat::Bgra) => {
                bgra_to_nv12(width, height, data, &mut self.yuv, &mut self.nv12);
                memory_sample(&self.nv12, time, duration)?
            }
        };
        self.frame_index += 1;

        let session = self.session.as_mut().expect("session opened above");
        let result = session.encode(&sample);
        if result.is_err() {
            // Start over (with a keyframe) if the caller keeps using us.
            self.session = None;
        }
        result
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        self.session = None;
        unsafe {
            let _ = MFShutdown();
        }
    }
}

impl Session {
    fn encode(&mut self, sample: &IMFSample) -> Result<Option<Encoded>, CodecError> {
        // Wait until the transform accepts input (and, for textures, has few
        // enough frames in flight). Outputs of earlier frames that arrive
        // meanwhile are kept: every one of them must be sent.
        let deadline = Instant::now() + EVENT_TIMEOUT;
        let mut ready = None;
        while self.need_input == 0 || (self.gpu_input && self.in_flight >= MAX_TEXTURES_IN_FLIGHT) {
            merge(&mut ready, self.next_event(deadline)?);
        }
        unsafe { self.transform.ProcessInput(self.input_id, sample, 0) }
            .map_err(|e| hw("ProcessInput", e))?;
        self.need_input -= 1;
        self.in_flight += 1;

        // Usually this frame's output comes next. If the transform asks for
        // more input first, it keeps frames in flight; `pending()` then tells
        // the caller to come back even if the screen stays still.
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while self.in_flight > 0 && self.need_input == 0 {
            merge(&mut ready, self.next_event(deadline)?);
        }
        Ok(ready)
    }

    fn drain(&mut self) -> Result<Option<Encoded>, CodecError> {
        // Most encoders just need a moment.
        let mut ready = None;
        let soft_deadline = Instant::now() + DRAIN_WAIT;
        while self.in_flight > 0 && Instant::now() < soft_deadline {
            merge(&mut ready, self.poll_event(soft_deadline)?);
        }
        if self.in_flight == 0 {
            return Ok(ready);
        }
        // Others hold frames until told to flush.
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
            .map_err(|e| hw("drain", e))?;
        self.draining = true;
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while self.draining {
            merge(&mut ready, self.next_event(deadline)?);
        }
        self.in_flight = 0;
        // A drained transform must be restarted; the NeedInput events it sent
        // before the drain are stale, so discard them and ask for fresh ones.
        self.need_input = 0;
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
        }
        .map_err(|e| hw("restart after drain", e))?;
        Ok(ready)
    }

    /// Asks Media Foundation for the next event; [`EventSink`] receives it.
    /// Only one request may be outstanding, so this follows each delivery.
    fn request_event(&self) -> Result<(), CodecError> {
        unsafe { self.events.BeginGetEvent(&self.callback, None) }
            .map_err(|e| hw("BeginGetEvent", e))
    }

    /// The type of the next event, or `None` if none arrives by `deadline`.
    ///
    /// The callback wakes this thread as soon as the event exists (no polling
    /// sleeps, whose ~15 ms granularity on Windows would add latency), and the
    /// deadline keeps a stuck driver from hanging the host.
    fn wait_event(&mut self, deadline: Instant) -> Result<Option<u32>, CodecError> {
        let kind = self.wait_event_status(deadline)?;
        Ok(match kind {
            Some((kind, status)) if status.is_err() => {
                return Err(CodecError::Hardware(format!(
                    "evento {kind} com erro: {}",
                    windows::core::Error::from(status)
                )));
            }
            Some((kind, _)) => {
                let known = [
                    METransformNeedInput.0,
                    METransformHaveOutput.0,
                    METransformDrainComplete.0,
                ];
                if !known.contains(&(kind as i32)) {
                    tracing::debug!("evento {kind} do encoder");
                }
                Some(kind)
            }
            None => None,
        })
    }

    fn wait_event_status(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<(u32, windows::core::HRESULT)>, CodecError> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        match self.event_rx.recv_timeout(timeout) {
            Ok(Ok(kind)) => {
                self.request_event()?;
                Ok(Some(kind))
            }
            Ok(Err(e)) => Err(hw("GetEvent", e)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(CodecError::Hardware("event callback gone".into()))
            }
        }
    }

    /// Like [`Session::next_event`], but a timeout just means "nothing yet".
    fn poll_event(&mut self, deadline: Instant) -> Result<Option<Encoded>, CodecError> {
        match self.wait_event(deadline)? {
            Some(kind) => self.handle(kind),
            None => Ok(None),
        }
    }

    /// Waits for the next transform event and handles it; returns an encoded
    /// frame if the event delivered one. Fails if the encoder stays silent
    /// past `deadline`, so `Auto` falls back to software.
    fn next_event(&mut self, deadline: Instant) -> Result<Option<Encoded>, CodecError> {
        match self.wait_event(deadline)? {
            Some(kind) => self.handle(kind),
            None => Err(CodecError::Hardware("o encoder parou de responder".into())),
        }
    }

    fn handle(&mut self, kind: u32) -> Result<Option<Encoded>, CodecError> {
        if kind == METransformDrainComplete.0 as u32 {
            self.draining = false;
            Ok(None)
        } else if kind == METransformNeedInput.0 as u32 {
            self.need_input += 1;
            Ok(None)
        } else if kind == METransformHaveOutput.0 as u32 {
            self.take_output()
        } else {
            Ok(None)
        }
    }

    fn take_output(&mut self) -> Result<Option<Encoded>, CodecError> {
        let mut sample = None;
        // A format change is answered by accepting the new type and asking for
        // the same output again, once the transform announces it (or soon).
        for _ in 0..6 {
            let provided = if self.provides_samples {
                None
            } else {
                Some(empty_sample(self.output_size)?)
            };
            let mut buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.output_id,
                pSample: ManuallyDrop::new(provided),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0;
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status)
            };
            // Take back ownership so the COM references are released.
            let output = ManuallyDrop::into_inner(buffer.pSample);
            drop(ManuallyDrop::into_inner(buffer.pEvents));

            match result {
                Ok(()) => {
                    sample = output;
                    break;
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => unsafe {
                    let new_type = self
                        .transform
                        .GetOutputAvailableType(self.output_id, 0)
                        .map_err(|e| hw("new output type", e))?;
                    self.transform
                        .SetOutputType(self.output_id, &new_type, 0)
                        .map_err(|e| hw("SetOutputType after change", e))?;
                    self.refresh_sequence_header();
                    // The new type may come with other output buffers (who
                    // provides them, how big): ask again, as FFmpeg does.
                    let info = self
                        .transform
                        .GetOutputStreamInfo(self.output_id)
                        .map_err(|e| hw("stream info after change", e))?;
                    tracing::info!(
                        "formato de saída mudou (flags {:#x}, {} bytes)",
                        info.dwFlags,
                        info.cbSize
                    );
                    let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                        | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                        as u32;
                    self.provides_samples = info.dwFlags & provides != 0 && !self.own_output_sample;
                    self.output_size = info.cbSize.max(self.width * self.height * 3 / 2);
                    // An async transform announces the output again, and
                    // asking before that is "unexpected" (Intel Quick Sync);
                    // others expect to be asked right away.
                    let deadline = Instant::now() + AFTER_FORMAT_CHANGE;
                    loop {
                        match self.wait_event(deadline)? {
                            Some(kind) if kind == METransformHaveOutput.0 as u32 => break,
                            Some(kind) if kind == METransformNeedInput.0 as u32 => {
                                self.need_input += 1;
                            }
                            Some(kind) if kind == METransformDrainComplete.0 as u32 => {
                                self.draining = false;
                            }
                            Some(_) => {}
                            None => break,
                        }
                    }
                },
                Err(e) => {
                    let context = format!(
                        "ProcessOutput ({} na fila, {} pedidos)",
                        self.in_flight, self.need_input
                    );
                    return Err(hw(&context, e));
                }
            }
        }
        let Some(sample) = sample else {
            return Err(CodecError::Hardware("output format kept changing".into()));
        };
        self.in_flight = self.in_flight.saturating_sub(1);

        let mut data = read_sample(&sample)?;
        // One pass over the NALs instead of three.
        let scan = h264::scan(&data);
        // Not every driver sets the clean-point attribute: look at the NALs too.
        let clean_point =
            unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0;
        let keyframe = clean_point || scan.has_idr;
        if keyframe && !scan.has_sps && !self.sequence_header.is_empty() {
            let mut with_header = self.sequence_header.clone();
            with_header.extend_from_slice(&data);
            data = with_header;
            // Checked on every SPS, so a profile switch later on is caught too.
            if let Some(profile) = h264::sps_profile(&data)
                && profile != PROFILE_BASELINE
            {
                return Err(profile_error(profile));
            }
        } else if let Some(profile) = scan.profile
            && profile != PROFILE_BASELINE
        {
            return Err(profile_error(profile));
        }
        Ok(Some(Encoded { data, keyframe }))
    }

    fn refresh_sequence_header(&mut self) {
        self.sequence_header = unsafe { self.transform.GetOutputCurrentType(self.output_id) }
            .ok()
            .and_then(|t| blob(&t, &MF_MT_MPEG_SEQUENCE_HEADER))
            .unwrap_or_default();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Async transforms hold driver resources until shut down explicitly.
        unsafe {
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
            let _ = self.activate.ShutdownObject();
        }
    }
}

/// Receives Media Foundation's asynchronous event deliveries (on an MF work
/// thread) and forwards each event's type to the encoding thread.
#[implement(IMFAsyncCallback)]
struct EventSink {
    generator: IMFMediaEventGenerator,
    tx: mpsc::Sender<windows::core::Result<(u32, windows::core::HRESULT)>>,
}

impl IMFAsyncCallback_Impl for EventSink_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        // Default work queue and behavior.
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, result: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        // The type, and the status the transform gave it (an error shows
        // up there before any call fails).
        let kind = unsafe { self.generator.EndGetEvent(result.ok()?) }
            .and_then(|event| unsafe { Ok((event.GetType()?, event.GetStatus()?)) });
        // The receiver is gone once the session ended; nothing to do then.
        let _ = self.tx.send(kind);
        Ok(())
    }
}

/// Hardware H.264 encoders that accept NV12, best first; with `device`,
/// only those on its adapter.
fn enumerate(device: Option<&ID3D11Device>) -> Result<Vec<IMFActivate>, CodecError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        match device {
            Some(device) => MFTEnum2(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                Some(&input),
                Some(&output),
                &adapter_filter(device)?,
                &mut array,
                &mut count,
            )
            .map_err(|e| hw("MFTEnum2", e))?,
            None => MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                Some(&input),
                Some(&output),
                &mut array,
                &mut count,
            )
            .map_err(|e| hw("MFTEnumEx", e))?,
        }
    }
    if array.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: the enumeration returned `count` activation objects in a
    // CoTaskMem array; each is moved out once, then the array itself is freed.
    let activates = unsafe {
        let items = (0..count as usize)
            .filter_map(|i| std::ptr::read(array.add(i)))
            .collect();
        CoTaskMemFree(Some(array as *const _));
        items
    };
    Ok(activates)
}

/// Enumeration attributes selecting the adapter `device` runs on.
fn adapter_filter(device: &ID3D11Device) -> Result<IMFAttributes, CodecError> {
    unsafe {
        let luid = device
            .cast::<IDXGIDevice>()
            .and_then(|dxgi| dxgi.GetAdapter())
            .and_then(|adapter| adapter.GetDesc())
            .map_err(|e| hw("adaptador da captura", e))?
            .AdapterLuid;
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1).map_err(|e| hw("MFCreateAttributes", e))?;
        let attributes =
            attributes.ok_or_else(|| CodecError::Hardware("MFCreateAttributes".into()))?;
        // A LUID blob: LowPart, then HighPart, both little-endian.
        let mut blob = luid.LowPart.to_le_bytes().to_vec();
        blob.extend_from_slice(&luid.HighPart.to_le_bytes());
        attributes
            .SetBlob(&MFT_ENUM_ADAPTER_LUID, &blob)
            .map_err(|e| hw("MFT_ENUM_ADAPTER_LUID", e))?;
        Ok(attributes)
    }
}

/// Gives the transform the capture's D3D11 device, so it reads the
/// capture's textures directly. Fails if the transform cannot.
fn attach_device(
    transform: &IMFTransform,
    device: &ID3D11Device,
) -> Result<IMFDXGIDeviceManager, CodecError> {
    unsafe {
        let aware = transform
            .GetAttributes()
            .and_then(|attributes| attributes.GetUINT32(&MF_SA_D3D11_AWARE))
            .unwrap_or(0);
        if aware == 0 {
            return Err(CodecError::Hardware("sem suporte a Direct3D 11".into()));
        }
        let mut token = 0u32;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)
            .map_err(|e| hw("MFCreateDXGIDeviceManager", e))?;
        let manager =
            manager.ok_or_else(|| CodecError::Hardware("MFCreateDXGIDeviceManager".into()))?;
        manager
            .ResetDevice(device, token)
            .map_err(|e| hw("ResetDevice", e))?;
        transform
            .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
            .map_err(|e| hw("MFT_MESSAGE_SET_D3D_MANAGER", e))?;
        Ok(manager)
    }
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut name = PWSTR::null();
    let mut len = 0u32;
    unsafe {
        if activate
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut len)
            .is_err()
        {
            return "H.264 MFT".to_owned();
        }
        let text = name.to_string().unwrap_or_else(|_| "H.264 MFT".to_owned());
        CoTaskMemFree(Some(name.0 as *const _));
        text
    }
}

/// The first type of `subtype` the transform offers on a stream (input or
/// output), to be filled in: what some drivers need, instead of a type
/// made from scratch.
fn offered_type(
    transform: &IMFTransform,
    stream: u32,
    output: bool,
    subtype: &GUID,
) -> Result<IMFMediaType, CodecError> {
    for index in 0..64 {
        let offered = unsafe {
            if output {
                transform.GetOutputAvailableType(stream, index)
            } else {
                transform.GetInputAvailableType(stream, index)
            }
        };
        let Ok(offered) = offered else { break };
        if unsafe { offered.GetGUID(&MF_MT_SUBTYPE) }.is_ok_and(|g| g == *subtype) {
            return Ok(offered);
        }
    }
    Err(CodecError::Hardware(format!(
        "o encoder não oferece o tipo {subtype:?} ({})",
        if output { "saída" } else { "entrada" }
    )))
}

/// Sets what a video type needs: `subtype`, size, frame rate, square
/// pixels, progressive.
fn fill_video_type(
    media_type: &IMFMediaType,
    subtype: &GUID,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<(), CodecError> {
    unsafe {
        let set = |r: windows::core::Result<()>, what| r.map_err(|e| hw(what, e));
        set(
            media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video),
            "major type",
        )?;
        set(media_type.SetGUID(&MF_MT_SUBTYPE, subtype), "subtype")?;
        set(
            media_type.SetUINT64(
                &MF_MT_FRAME_SIZE,
                (u64::from(width) << 32) | u64::from(height),
            ),
            "frame size",
        )?;
        set(
            media_type.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1),
            "frame rate",
        )?;
        set(
            media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1),
            "aspect ratio",
        )?;
        set(
            media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32),
            "interlace mode",
        )?;
        Ok(())
    }
}

/// An empty sample with room for `size` bytes, for transforms that want the
/// caller to provide output samples.
fn empty_sample(size: u32) -> Result<IMFSample, CodecError> {
    unsafe {
        let buffer = MFCreateAlignedMemoryBuffer(size, MF_16_BYTE_ALIGNMENT)
            .map_err(|e| hw("MFCreateAlignedMemoryBuffer", e))?;
        let sample = MFCreateSample().map_err(|e| hw("MFCreateSample", e))?;
        sample.AddBuffer(&buffer).map_err(|e| hw("AddBuffer", e))?;
        Ok(sample)
    }
}

/// A sample wrapping an NV12 texture, read by the transform on the GPU.
fn texture_sample(
    texture: &ID3D11Texture2D,
    time: i64,
    duration: i64,
) -> Result<IMFSample, CodecError> {
    unsafe {
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)
            .map_err(|e| hw("MFCreateDXGISurfaceBuffer", e))?;
        // Some encoders check the buffer's length even for textures.
        let length = buffer
            .cast::<IMF2DBuffer>()
            .and_then(|buffer| buffer.GetContiguousLength())
            .map_err(|e| hw("GetContiguousLength", e))?;
        buffer
            .SetCurrentLength(length)
            .map_err(|e| hw("SetCurrentLength", e))?;
        let sample = MFCreateSample().map_err(|e| hw("MFCreateSample", e))?;
        sample.AddBuffer(&buffer).map_err(|e| hw("AddBuffer", e))?;
        sample
            .SetSampleTime(time)
            .map_err(|e| hw("SetSampleTime", e))?;
        sample
            .SetSampleDuration(duration)
            .map_err(|e| hw("SetSampleDuration", e))?;
        Ok(sample)
    }
}

/// A sample holding a copy of `data` in system memory.
pub(crate) fn memory_sample(
    data: &[u8],
    time: i64,
    duration: i64,
) -> Result<IMFSample, CodecError> {
    unsafe {
        let buffer =
            // Aligned as FFmpeg does: some drivers read the image in blocks.
            MFCreateAlignedMemoryBuffer(data.len() as u32, MF_16_BYTE_ALIGNMENT)
                .map_err(|e| hw("MFCreateAlignedMemoryBuffer", e))?;
        let mut ptr = std::ptr::null_mut();
        buffer
            .Lock(&mut ptr, None, None)
            .map_err(|e| hw("Lock", e))?;
        // SAFETY: the buffer was created with room for `data.len()` bytes.
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        buffer.Unlock().map_err(|e| hw("Unlock", e))?;
        buffer
            .SetCurrentLength(data.len() as u32)
            .map_err(|e| hw("SetCurrentLength", e))?;
        let sample = MFCreateSample().map_err(|e| hw("MFCreateSample", e))?;
        sample.AddBuffer(&buffer).map_err(|e| hw("AddBuffer", e))?;
        sample
            .SetSampleTime(time)
            .map_err(|e| hw("SetSampleTime", e))?;
        sample
            .SetSampleDuration(duration)
            .map_err(|e| hw("SetSampleDuration", e))?;
        Ok(sample)
    }
}

fn read_sample(sample: &IMFSample) -> Result<Vec<u8>, CodecError> {
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| hw("ConvertToContiguousBuffer", e))?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer
            .Lock(&mut ptr, None, Some(&mut len))
            .map_err(|e| hw("Lock output", e))?;
        // SAFETY: Lock returned a pointer to `len` valid bytes until Unlock.
        let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        buffer.Unlock().map_err(|e| hw("Unlock output", e))?;
        Ok(data)
    }
}

fn blob(media_type: &IMFMediaType, key: &GUID) -> Option<Vec<u8>> {
    unsafe {
        let size = media_type.GetBlobSize(key).ok()?;
        let mut data = vec![0; size as usize];
        media_type.GetBlob(key, &mut data, None).ok()?;
        Some(data)
    }
}

/// Applies an encoder setting; returns whether the driver accepted it.
pub(crate) fn set(codec: &ICodecAPI, api: &GUID, value: VARIANT) -> bool {
    match unsafe { codec.SetValue(api, &value) } {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!("ICodecAPI {api:?} não suportado: {e}");
            false
        }
    }
}

fn variant(vt: VARENUM, value: VARIANT_0_0_0) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: value,
            }),
        },
    }
}

fn u32_variant(value: u32) -> VARIANT {
    variant(VT_UI4, VARIANT_0_0_0 { ulVal: value })
}

pub(crate) fn bool_variant() -> VARIANT {
    variant(
        VT_BOOL,
        VARIANT_0_0_0 {
            boolVal: VARIANT_TRUE,
        },
    )
}

/// How one encoder did one way (see [`probe`]).
#[derive(Debug)]
pub struct ProbeResult {
    pub encoder: String,
    pub variant: String,
    /// Frames out and the time per frame, or the error.
    pub result: Result<String, String>,
}

/// How a probe feeds the encoder.
#[derive(Clone, Copy)]
enum Feed {
    /// From memory.
    Memory,
    /// From memory, with a device of the adapter attached.
    MemoryWithDevice,
    /// NV12 textures of the adapter, with these bind flags.
    Texture(u32),
}

/// Tries every hardware H.264 encoder several ways, encoding a few
/// synthetic frames: what a driver accepts, to diagnose a machine where
/// none works. First enumerated as a whole (as encoding starts out), then
/// adapter by adapter, with a device of that adapter.
pub fn probe(width: u32, height: u32) -> Result<Vec<ProbeResult>, CodecError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_VIDEO_ENCODER,
    };

    let settings = crate::EncoderSettings::default();
    // Started here so the enumeration below can run.
    let first = MfEncoder::new(settings)?;
    let mut names: Vec<String> = Vec::new();
    for name in enumerate(None)?.iter().map(friendly_name) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let adapters = adapters();
    drop(first);

    let none = Tweaks {
        low_latency: false,
        rate_control: false,
        gop: false,
        profile: false,
        offered_types: false,
        own_output_sample: false,
        start_message: true,
    };
    let offered = Tweaks {
        offered_types: true,
        ..Tweaks::ALL
    };
    let mut results = Vec::new();
    for name in &names {
        for (variant, tweaks, (w, h)) in [
            (
                "memória, todas as configurações",
                Tweaks::ALL,
                (width, height),
            ),
            ("memória, nenhuma opcional", none, (width, height)),
            // Encoders work in 16-line blocks: 1080 is not a multiple.
            ("memória, 1920x1088", Tweaks::ALL, (1920, 1088)),
            ("memória, 1280x720", Tweaks::ALL, (1280, 720)),
            ("memória, tipos do encoder", offered, (width, height)),
            (
                "memória, buffer de saída próprio",
                Tweaks {
                    own_output_sample: true,
                    ..Tweaks::ALL
                },
                (width, height),
            ),
            (
                "memória, sem início de stream",
                Tweaks {
                    start_message: false,
                    ..Tweaks::ALL
                },
                (width, height),
            ),
            (
                "memória, tipos do encoder, mínimo",
                Tweaks {
                    offered_types: true,
                    ..none
                },
                (width, height),
            ),
        ] {
            let result = probe_one(settings, name, None, Feed::Memory, tweaks, w, h);
            results.push(ProbeResult {
                encoder: name.clone(),
                variant: variant.into(),
                result,
            });
        }
    }
    let shader = (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32;
    let encoder_bind = D3D11_BIND_VIDEO_ENCODER.0 as u32;
    for (adapter, device) in adapters {
        let mut on_adapter: Vec<String> = match enumerate(Some(&device)) {
            Ok(list) => list.iter().map(friendly_name).collect(),
            Err(e) => {
                results.push(ProbeResult {
                    encoder: format!("(placa {adapter})"),
                    variant: "listar codificadores".into(),
                    result: Err(e.to_string()),
                });
                continue;
            }
        };
        on_adapter.sort();
        on_adapter.dedup();
        if on_adapter.is_empty() {
            results.push(ProbeResult {
                encoder: format!("(placa {adapter})"),
                variant: "listar codificadores".into(),
                result: Err("nenhum codificador nesta placa".into()),
            });
        }
        for name in on_adapter {
            for (variant, feed, tweaks) in [
                (
                    "memória + dispositivo anexado",
                    Feed::MemoryWithDevice,
                    Tweaks::ALL,
                ),
                (
                    "textura (shader/render)",
                    Feed::Texture(shader),
                    Tweaks::ALL,
                ),
                (
                    "textura (video encoder)",
                    Feed::Texture(encoder_bind),
                    Tweaks::ALL,
                ),
                ("textura, tipos do encoder", Feed::Texture(shader), offered),
            ] {
                let result = probe_one(settings, &name, Some(&device), feed, tweaks, width, height);
                results.push(ProbeResult {
                    encoder: format!("{name} na placa {adapter}"),
                    variant: variant.into(),
                    result,
                });
            }
        }
    }
    Ok(results)
}

/// Every hardware adapter, with a device on it (video support).
fn adapters() -> Vec<(String, ID3D11Device)> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
        D3D11CreateDevice, ID3D11Multithread,
    };
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
    };

    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut seen = Vec::new();
    for a in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(a) }) else {
            break;
        };
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        // The same GPU can be listed more than once.
        let luid = (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart);
        if seen.contains(&luid) {
            continue;
        }
        seen.push(luid);
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
        let name = String::from_utf16_lossy(&desc.Description[..len]);
        let mut device = None;
        let created = unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        };
        if let (Ok(()), Some(device)) = (created, device) {
            if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
                let _ = unsafe { multithread.SetMultithreadProtected(true) };
            }
            found.push((name, device));
        }
    }
    found
}

/// Encodes 30 synthetic frames with encoder `name`, fed as `feed`.
fn probe_one(
    settings: EncoderSettings,
    name: &str,
    device: Option<&ID3D11Device>,
    feed: Feed,
    tweaks: Tweaks,
    width: u32,
    height: u32,
) -> Result<String, String> {
    const FRAMES: usize = 30;
    let mut encoder = MfEncoder::new(settings).map_err(|e| e.to_string())?;
    encoder.only = Some(name.to_owned());
    encoder.tweaks = tweaks;
    if let Feed::MemoryWithDevice = feed {
        encoder.memory_device = device.cloned();
    }
    // A moving gradient, so every frame has something to encode.
    let pixels = width as usize * height as usize;
    let frame = |n: usize| -> Vec<u8> {
        let mut data = vec![128u8; pixels * 3 / 2];
        for (i, y) in data[..pixels].iter_mut().enumerate() {
            *y = ((i % width as usize + n * 8) % 256) as u8;
        }
        data
    };
    let started = std::time::Instant::now();
    let mut out = 0;
    for n in 0..FRAMES {
        let data = frame(n);
        let texture;
        let pixels = match (feed, device) {
            (Feed::Texture(bind), Some(device)) => {
                texture = nv12_texture(device, width, height, bind, &data)?;
                Pixels::D3d11(&texture)
            }
            _ => Pixels::Memory(&data),
        };
        let image = Image {
            width,
            height,
            format: PixelFormat::Nv12,
            pixels,
        };
        match encoder.encode(&image) {
            Ok(Some(_)) => out += 1,
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if out == 0 {
        return Err("nenhum quadro saiu".into());
    }
    let zero_copy = encoder.session.as_ref().is_some_and(|s| s.gpu_input);
    Ok(format!(
        "{out}/{FRAMES} quadros, {:.1} ms por quadro{}",
        started.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64,
        if zero_copy { ", zero-copy" } else { "" }
    ))
}

/// An NV12 texture on `device` holding `data`.
fn nv12_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    bind: u32,
    data: &[u8],
) -> Result<ID3D11Texture2D, String> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    // NV12 initial data: the Y plane, then the UV plane, same pitch.
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr().cast(),
        SysMemPitch: width,
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    unsafe { device.CreateTexture2D(&desc, Some(&initial), Some(&mut texture)) }
        .map_err(|e| format!("criar textura NV12: {e}"))?;
    texture.ok_or_else(|| "criar textura NV12".to_owned())
}
