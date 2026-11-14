//! X11 capture (F7, milestone 4).
//!
//! RandR lists the monitors; each capture reads its monitor out of the root
//! window with MIT-SHM (`XShmGetImage` into a memory segment shared with the
//! X server), or with a plain `GetImage` where shared memory is not offered
//! (a remote X server). Damage tells when something was drawn, so a still
//! screen costs no copy; without it, each frame is compared with the last.
//! The cursor is not in the image (X draws it apart): the viewer shows its
//! own, as on the other systems.
//!
//! Coordinates: the main monitor (RandR's primary) is at (0, 0), as on
//! Windows, and the others where they sit from it (negative to the left or
//! above). X11 itself counts from the top-left corner of the whole screen;
//! [`origin`] is the main monitor's place there, which input adds back.

use std::os::fd::{AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder, Window};
use x11rb::rust_connection::RustConnection;

use crate::{CaptureError, Capturer, Display, Frame, PixelFormat, Rect, even};

/// Longest a still screen goes without a full comparison, in case a change
/// came with no damage report (some drivers, some compositors).
const RECHECK: Duration = Duration::from_secs(1);

/// How often damage reports are looked at while waiting for a change.
const POLL: Duration = Duration::from_millis(4);

fn platform(context: &str, error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Platform(format!("{context}: {error}"))
}

/// Whether this is a Wayland session, where X11 sees only its own windows.
pub fn is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind.eq_ignore_ascii_case("wayland"))
        || (std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_some())
}

/// Connects to the X server of this session (`$DISPLAY`).
pub(crate) fn connect() -> Result<(RustConnection, usize), CaptureError> {
    if std::env::var_os("DISPLAY").is_none_or(|d| d.is_empty()) {
        return Err(CaptureError::Platform(
            "sem sessão gráfica X11 (a variável DISPLAY está vazia)".into(),
        ));
    }
    RustConnection::connect(None).map_err(|e| platform("conectar ao X11", e))
}

/// A monitor as RandR describes it, in X11 coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// The displays for `monitors` on a screen of `screen` pixels, and the main
/// one's place in X11 coordinates. The main one is RandR's primary, else the
/// one at the corner, else the top-left one. No monitors (an X server with
/// no RandR, a virtual one): the whole screen is one.
pub(crate) fn layout(monitors: &[Monitor], screen: (u32, u32)) -> (Vec<Display>, (i32, i32)) {
    let monitors: Vec<Monitor> = monitors
        .iter()
        .filter(|m| m.width >= 2 && m.height >= 2)
        .cloned()
        .collect();
    if monitors.is_empty() {
        let whole = Display {
            id: "tela".into(),
            rect: Rect {
                x: 0,
                y: 0,
                width: screen.0,
                height: screen.1,
            },
            virtual_display: false,
        };
        return (vec![whole], (0, 0));
    }
    let main = monitors
        .iter()
        .find(|m| m.primary)
        .or_else(|| monitors.iter().find(|m| (m.x, m.y) == (0, 0)))
        .or_else(|| monitors.iter().min_by_key(|m| (m.y, m.x)))
        .expect("not empty");
    let origin = (main.x, main.y);
    let mut ids: Vec<String> = Vec::new();
    let displays = monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            // Names tell monitors apart; a missing or repeated one gets a number.
            let mut id = m.name.trim().to_owned();
            if id.is_empty() || ids.contains(&id) {
                id = format!("monitor {}", i + 1);
            }
            ids.push(id.clone());
            Display {
                id,
                rect: Rect {
                    x: m.x - origin.0,
                    y: m.y - origin.1,
                    width: m.width,
                    height: m.height,
                },
                virtual_display: false,
            }
        })
        .collect();
    (displays, origin)
}

/// The monitors RandR (1.5) lists as active; empty without it.
fn monitors(conn: &RustConnection, root: Window) -> Vec<Monitor> {
    let supported = conn
        .randr_query_version(1, 5)
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 5));
    if !supported {
        return Vec::new();
    }
    let Some(reply) = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return Vec::new();
    };
    reply
        .monitors
        .iter()
        .map(|m| Monitor {
            name: conn
                .get_atom_name(m.name)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                .unwrap_or_default(),
            x: m.x.into(),
            y: m.y.into(),
            width: m.width.into(),
            height: m.height.into(),
            primary: m.primary,
        })
        .collect()
}

/// The displays of this X screen and the main one's place.
fn current(conn: &RustConnection, screen: usize) -> (Vec<Display>, (i32, i32), Window) {
    let root = &conn.setup().roots[screen];
    let size = (
        u32::from(root.width_in_pixels),
        u32::from(root.height_in_pixels),
    );
    let (displays, origin) = layout(&monitors(conn, root.root), size);
    (displays, origin, root.root)
}

pub fn displays() -> Result<Vec<Display>, CaptureError> {
    let (conn, screen) = connect()?;
    Ok(current(&conn, screen).0)
}

/// Where the main monitor is in X11 coordinates (input adds it back).
pub fn origin() -> (i32, i32) {
    connect()
        .map(|(conn, screen)| current(&conn, screen).1)
        .unwrap_or_default()
}

/// Checks that the screen's pixels are 32-bit BGRX in memory, the only
/// layout read here (24-bit colour on every desktop of the last 15 years).
fn check_format(conn: &RustConnection, screen: usize) -> Result<(), CaptureError> {
    let setup = conn.setup();
    let root = &setup.roots[screen];
    let bpp = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == root.root_depth)
        .map(|f| f.bits_per_pixel);
    let visual = root
        .allowed_depths
        .iter()
        .flat_map(|d| d.visuals.iter())
        .find(|v| v.visual_id == root.root_visual);
    let bgrx = visual.is_some_and(|v| {
        (v.red_mask, v.green_mask, v.blue_mask) == (0x00ff_0000, 0x0000_ff00, 0x0000_00ff)
    });
    if bpp != Some(32) || !bgrx || setup.image_byte_order != ImageOrder::LSB_FIRST {
        return Err(CaptureError::Platform(format!(
            "tela X11 em profundidade {} ({bpp:?} bits por pixel): só 24 bits são capturados",
            root.root_depth
        )));
    }
    Ok(())
}

/// A memory segment shared with the X server.
struct Segment {
    id: shm::Seg,
    ptr: *mut u8,
    len: usize,
    /// Keeps the memory's file open for as long as it is mapped.
    fd: OwnedFd,
}

impl Segment {
    /// Makes a segment of `len` bytes and hands it to the X server.
    #[allow(unsafe_code)] // memfd_create and mmap: plain libc calls, checked below
    fn new(conn: &RustConnection, len: usize) -> Result<Self, CaptureError> {
        let fd = unsafe { libc::memfd_create(c"hext-captura".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(platform("memfd_create", std::io::Error::last_os_error()));
        }
        // SAFETY: a new descriptor, owned by nothing else.
        let fd = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let file = std::fs::File::from(fd);
        file.set_len(len as u64)
            .map_err(|e| platform("ftruncate", e))?;
        // SAFETY: maps the whole file just sized; unmapped in Drop.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(platform("mmap", std::io::Error::last_os_error()));
        }
        let fd = OwnedFd::from(file);
        let mut segment = Self {
            id: 0,
            ptr: ptr.cast(),
            len,
            fd,
        };
        let id = conn.generate_id().map_err(|e| platform("XShm", e))?;
        let sent = segment.fd.try_clone().map_err(|e| platform("dup", e))?;
        conn.shm_attach_fd(id, sent, false)
            .map_err(|e| platform("XShmAttach", e))?
            .check()
            .map_err(|e| platform("XShmAttach", e))?;
        segment.id = id;
        Ok(segment)
    }

    #[allow(unsafe_code)] // reads the mapping made in `new`
    fn bytes(&self) -> &[u8] {
        // SAFETY: `len` bytes mapped in `new`, alive until Drop. The X server
        // writes only during a GetImage, which has returned by now.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    #[allow(unsafe_code)] // undoes the mmap of `new`
    fn unmap(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: the mapping from `new`, unmapped once.
            unsafe { libc::munmap(self.ptr.cast(), self.len) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        self.unmap();
    }
}

/// Captures one monitor of the X screen.
pub struct X11Capturer {
    conn: RustConnection,
    root: Window,
    /// The monitor, in X11 coordinates.
    source: Rect,
    /// The monitor, as sessions see it (main one at the origin).
    rect: Rect,
    segment: Option<Segment>,
    damage: Option<damage::Damage>,
    frame: Option<Frame>,
    /// The previous image's pixels, reused for the next one.
    spare: Vec<u8>,
    interval: Duration,
    next_at: Instant,
    checked_at: Instant,
    copy_time: Duration,
}

impl X11Capturer {
    /// Opens display `id` ([`Display::id`]).
    pub fn new(id: &str, max_fps: u32) -> Result<Self, CaptureError> {
        let (conn, screen) = connect()?;
        check_format(&conn, screen)?;
        let (displays, origin, root) = current(&conn, screen);
        let display = displays
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| CaptureError::DisplayGone(id.into()))?;
        let rect = display.rect;
        let source = Rect {
            x: rect.x + origin.0,
            y: rect.y + origin.1,
            ..rect
        };
        let len = source.width as usize * source.height as usize * 4;
        let segment = if conn
            .extension_information(shm::X11_EXTENSION_NAME)
            .ok()
            .flatten()
            .is_some()
            && conn
                .shm_query_version()
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 2))
        {
            Segment::new(&conn, len)
                .inspect_err(|e| tracing::info!("sem memória compartilhada com o X11: {e}"))
                .ok()
        } else {
            None
        };
        let damage = watch_damage(&conn, root)
            .inspect_err(|e| tracing::info!("sem a extensão Damage: {e}"))
            .ok();
        tracing::info!(
            "capturando o monitor {id} (X11{}{}): {}x{} em ({}, {})",
            if segment.is_some() { ", XShm" } else { "" },
            if damage.is_some() { ", Damage" } else { "" },
            source.width,
            source.height,
            source.x,
            source.y
        );
        let now = Instant::now();
        Ok(Self {
            conn,
            root,
            source,
            rect,
            segment,
            damage,
            frame: None,
            spare: Vec::new(),
            interval: Duration::from_secs(1) / max_fps.clamp(1, 240),
            next_at: now,
            checked_at: now,
            copy_time: Duration::ZERO,
        })
    }

    /// Whether something was drawn since the last look (`true` without
    /// Damage: only a comparison tells).
    fn damaged(&mut self) -> Result<bool, CaptureError> {
        let Some(damage) = self.damage else {
            return Ok(true);
        };
        let mut any = false;
        while let Some(event) = self.conn.poll_for_event().map_err(|e| platform("X11", e))? {
            any |= matches!(event, Event::DamageNotify(_));
        }
        if any {
            // What comes after this is the next change.
            self.conn
                .damage_subtract(damage, x11rb::NONE, x11rb::NONE)
                .map_err(|e| platform("DamageSubtract", e))?;
            self.conn.flush().map_err(|e| platform("X11", e))?;
        }
        Ok(any)
    }

    /// Reads the monitor into `out` (BGRX, `even(width) * 4` bytes a row).
    fn grab(&mut self, out: &mut Vec<u8>) -> Result<(), CaptureError> {
        let Rect {
            x,
            y,
            width,
            height,
        } = self.source;
        let (w, h) = (width as usize, height as usize);
        let gone = |e: &dyn std::fmt::Display| {
            tracing::debug!("captura X11: {e}");
            CaptureError::DisplayGone(format!("{}x{} em ({x}, {y})", width, height))
        };
        let (cx, cy) = (x as i16, y as i16);
        let (cw, ch) = (width as u16, height as u16);
        let format = u8::from(ImageFormat::Z_PIXMAP);
        out.clear();
        let row = even(width) as usize * 4;
        out.reserve(row * even(height) as usize);
        match &self.segment {
            Some(segment) => {
                self.conn
                    .shm_get_image(self.root, cx, cy, cw, ch, !0, format, segment.id, 0)
                    .map_err(|e| platform("XShmGetImage", e))?
                    .reply()
                    .map_err(|e| gone(&e))?;
                let bytes = segment.bytes();
                for line in bytes.chunks_exact(w * 4).take(even(height) as usize) {
                    out.extend_from_slice(&line[..row]);
                }
            }
            None => {
                let reply = self
                    .conn
                    .get_image(ImageFormat::Z_PIXMAP, self.root, cx, cy, cw, ch, !0)
                    .map_err(|e| platform("GetImage", e))?
                    .reply()
                    .map_err(|e| gone(&e))?;
                if reply.data.len() < w * h * 4 {
                    return Err(platform("GetImage", "imagem menor que o monitor"));
                }
                for line in reply.data.chunks_exact(w * 4).take(even(height) as usize) {
                    out.extend_from_slice(&line[..row]);
                }
            }
        }
        Ok(())
    }
}

/// Asks for a report each time anything is drawn on the screen.
fn watch_damage(conn: &RustConnection, root: Window) -> Result<damage::Damage, CaptureError> {
    if conn
        .extension_information(damage::X11_EXTENSION_NAME)
        .map_err(|e| platform("X11", e))?
        .is_none()
    {
        return Err(CaptureError::Platform("não oferecida".into()));
    }
    // (Damage takes XFixes's regions; both want their versions asked first.)
    conn.xfixes_query_version(5, 0)
        .map_err(|e| platform("XFixes", e))?
        .reply()
        .map_err(|e| platform("XFixes", e))?;
    conn.damage_query_version(1, 1)
        .map_err(|e| platform("Damage", e))?
        .reply()
        .map_err(|e| platform("Damage", e))?;
    let id = conn.generate_id().map_err(|e| platform("Damage", e))?;
    conn.damage_create(id, root, damage::ReportLevel::NON_EMPTY)
        .map_err(|e| platform("DamageCreate", e))?
        .check()
        .map_err(|e| platform("DamageCreate", e))?;
    Ok(id)
}

impl Capturer for X11Capturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<bool, CaptureError> {
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            // Never more often than the frame rate asks.
            if self.next_at > now {
                let wait = (self.next_at - now).min(deadline.saturating_duration_since(now));
                if wait.is_zero() {
                    return Ok(false);
                }
                std::thread::sleep(wait);
                continue;
            }
            let recheck = self.checked_at.elapsed() >= RECHECK;
            if self.frame.is_none() || self.damaged()? || recheck {
                let started = Instant::now();
                let mut data = std::mem::take(&mut self.spare);
                self.grab(&mut data)?;
                self.checked_at = Instant::now();
                self.next_at = self.next_at.max(started - self.interval) + self.interval;
                // Damage said nothing, or there is no Damage: only a
                // different image is news.
                let same = self.frame.as_ref().is_some_and(|f| f.data == data);
                if same {
                    self.spare = data;
                } else {
                    let frame = Frame {
                        width: even(self.source.width),
                        height: even(self.source.height),
                        format: PixelFormat::Bgra,
                        data,
                    };
                    if let Some(old) = self.frame.replace(frame) {
                        self.spare = old.data;
                    }
                    self.copy_time = started.elapsed();
                    return Ok(true);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            std::thread::sleep(POLL.min(deadline - now));
        }
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

impl Drop for X11Capturer {
    fn drop(&mut self) {
        if let Some(damage) = self.damage {
            let _ = self.conn.damage_destroy(damage);
        }
        if let Some(segment) = &self.segment {
            let _ = self.conn.shm_detach(segment.id);
        }
        let _ = self.conn.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, x: i32, y: i32, width: u32, height: u32, primary: bool) -> Monitor {
        Monitor {
            name: name.into(),
            x,
            y,
            width,
            height,
            primary,
        }
    }

    #[test]
    fn the_primary_monitor_is_the_origin() {
        // A laptop (primary) with a monitor to its left.
        let (displays, origin) = layout(
            &[
                monitor("HDMI-1", 0, 0, 1920, 1080, false),
                monitor("eDP-1", 1920, 200, 1366, 768, true),
            ],
            (3286, 1280),
        );
        assert_eq!(origin, (1920, 200));
        assert_eq!(displays[0].id, "HDMI-1");
        assert_eq!((displays[0].rect.x, displays[0].rect.y), (-1920, -200));
        assert_eq!((displays[1].rect.x, displays[1].rect.y), (0, 0));
        // Numbered, the main one comes first.
        let numbered = crate::numbered(displays);
        assert_eq!(numbered[0].id, "eDP-1");
    }

    #[test]
    fn without_a_primary_the_corner_one_is_main() {
        let (displays, origin) = layout(
            &[
                monitor("DP-2", 2560, 0, 1920, 1080, false),
                monitor("DP-1", 0, 0, 2560, 1440, false),
            ],
            (4480, 1440),
        );
        assert_eq!(origin, (0, 0));
        assert_eq!(displays[0].rect.x, 2560);
        // No one at the corner: the top-left one.
        let (_, origin) = layout(
            &[
                monitor("A", 100, 50, 800, 600, false),
                monitor("B", 900, 0, 800, 600, false),
            ],
            (1700, 650),
        );
        assert_eq!(origin, (900, 0));
    }

    #[test]
    fn no_monitors_is_the_whole_screen_and_names_stay_unique() {
        let (displays, origin) = layout(&[], (1280, 800));
        assert_eq!(origin, (0, 0));
        assert_eq!(displays.len(), 1);
        assert_eq!(
            (displays[0].rect.width, displays[0].rect.height),
            (1280, 800)
        );
        let (displays, _) = layout(
            &[
                monitor("", 0, 0, 800, 600, true),
                monitor("X", 800, 0, 800, 600, false),
                monitor("X", 1600, 0, 800, 600, false),
                monitor("tiny", 0, 0, 0, 0, false),
            ],
            (2400, 600),
        );
        let ids: Vec<_> = displays.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["monitor 1", "X", "monitor 3"]);
    }

    /// No virtual monitor on Linux (not needed for F7): never offered.
    #[test]
    fn no_virtual_monitor_on_linux() {
        assert!(!crate::virtual_display::available());
        assert!(!crate::virtual_display::no_monitor());
    }

    /// Paints `rect` of the root window in `pixel` (0xRRGGBB).
    fn paint(conn: &RustConnection, root: Window, rect: (i16, i16, u16, u16), pixel: u32) {
        use x11rb::protocol::xproto::{CreateGCAux, Rectangle};
        let gc = conn.generate_id().unwrap();
        conn.create_gc(gc, root, &CreateGCAux::new().foreground(pixel))
            .unwrap()
            .check()
            .unwrap();
        let (x, y, width, height) = rect;
        conn.poly_fill_rectangle(
            root,
            gc,
            &[Rectangle {
                x,
                y,
                width,
                height,
            }],
        )
        .unwrap()
        .check()
        .unwrap();
        conn.free_gc(gc).unwrap();
        conn.flush().unwrap();
    }

    /// On a real X server (Xvfb in CI): what is drawn on the screen comes
    /// out in the capture, and a still screen gives no new frame.
    #[test]
    #[ignore = "needs an X server: xvfb-run cargo test -- --ignored x11_"]
    fn x11_captures_what_is_on_the_screen() {
        let (conn, screen) = connect().unwrap();
        let root = conn.setup().roots[screen].root;
        let list = crate::displays(false).unwrap();
        assert!(!list.is_empty());
        let main = &list[0];
        assert_eq!((main.rect.x, main.rect.y), (0, 0), "{list:?}");
        paint(&conn, root, (0, 0, 64, 64), 0x00ff_0000);
        paint(&conn, root, (64, 0, 64, 64), 0x0000_80ff);

        let mut capturer = crate::open(0, false, 30).unwrap();
        assert!(capturer.next_frame(Duration::from_secs(2)).unwrap());
        let frame = capturer.frame().unwrap().clone();
        assert_eq!(frame.format, PixelFormat::Bgra);
        assert_eq!(frame.width, even(main.rect.width));
        let px = |x: usize, y: usize| {
            let at = (y * frame.width as usize + x) * 4;
            [frame.data[at], frame.data[at + 1], frame.data[at + 2]]
        };
        // BGRA in memory.
        assert_eq!(px(10, 10), [0x00, 0x00, 0xff]);
        assert_eq!(px(100, 10), [0xff, 0x80, 0x00]);

        // Nothing drawn: nothing new.
        assert!(!capturer.next_frame(Duration::from_millis(300)).unwrap());
        // Drawn again: a new frame with it.
        paint(&conn, root, (0, 0, 64, 64), 0x0000_ff00);
        assert!(capturer.next_frame(Duration::from_secs(2)).unwrap());
        let frame = capturer.frame().unwrap();
        assert_eq!(&frame.data[..3], &[0x00, 0xff, 0x00]);
        assert!(capturer.copy_time() < Duration::from_secs(1));
    }
}
