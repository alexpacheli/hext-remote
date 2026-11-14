//! Each session's picture, drawn on a thread of its own.
//!
//! The windows of the viewer share one thread, the event loop's. Nothing
//! that waits on the GPU runs there any more: making the GPU (most of a
//! second, once per window), uploading pictures, and drawing them, which can
//! wait for the screen. On a Mac, a drawable stuck while the window was
//! minimized made that wait endless: the window beachballed until it was
//! forced to quit. Now only the picture would stop, and for a second at most
//! (see `hext_render::configure`).
//!
//! The decoder leaves the newest picture here and wakes the session's render
//! thread directly; the window only tells its size, its buttons, whether it
//! can be seen, and when the system asks it to paint.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use hext_codec::YuvFrame;
use hext_render::{ButtonAction, DisplayBar, Pending, RenderError, VideoRect};
use hext_session::Gaps;
use tracing::{debug, warn};

/// A frame this long in drawing is logged: the window keeps answering, but
/// the picture stood still.
const SLOW_RENDER: Duration = Duration::from_millis(100);
/// A frame the screen had no room for is tried again after this long.
const RETRY: Duration = Duration::from_millis(250);

/// What the window draws over the picture.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Overlay {
    pub button: Option<(ButtonAction, VideoRect)>,
    pub bar: Option<DisplayBar>,
    pub chat: Option<VideoRect>,
    pub handle: Option<VideoRect>,
}

/// What the render thread tells the window.
pub enum News {
    /// The picture on screen now shows this display of the host.
    Display(u8),
    /// There is no GPU to draw with.
    Failed(RenderError),
}

/// One session's picture: what the decoder and the window leave for the
/// render thread, and what it leaves for the window.
#[derive(Default)]
pub struct Screen {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    /// The newest picture, and the host's display it shows; older ones are
    /// dropped unseen.
    newest: Option<(YuvFrame, u8)>,
    /// The window's new size, in pixels.
    size: Option<(u32, u32)>,
    overlay: Option<Overlay>,
    /// The system asked the window to paint.
    redraw: bool,
    /// Minimized or covered: nothing is drawn (on a Mac, drawing then is
    /// what gets stuck).
    hidden: bool,
    quit: bool,
    /// The size of the video drawn, for the window to place the mouse.
    video: Option<(u32, u32)>,
    /// The display in the picture drawn; `None`: told again with the next.
    display: Option<u8>,
}

/// What the render thread does next.
#[derive(Default)]
struct Work {
    frame: Option<(YuvFrame, u8)>,
    size: Option<(u32, u32)>,
    overlay: Option<Overlay>,
    draw: bool,
}

impl Screen {
    fn with(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.state.lock().expect("lock"));
        self.wake.notify_one();
    }

    /// (Decoder) Leaves `frame`, of the host's display `display`.
    pub fn put(&self, frame: YuvFrame, display: u8) {
        self.with(|state| state.newest = Some((frame, display)));
    }

    /// The window has a new size.
    pub fn resize(&self, width: u32, height: u32) {
        self.with(|state| state.size = Some((width, height)));
    }

    /// What to draw over the picture changed.
    pub fn overlay(&self, overlay: Overlay) {
        self.with(|state| state.overlay = Some(overlay));
    }

    /// The system asks the window to paint.
    pub fn redraw(&self) {
        self.with(|state| state.redraw = true);
    }

    /// The window was minimized or covered (`true`), or is seen again.
    pub fn hide(&self, hidden: bool) {
        self.with(|state| {
            // Seen again: the newest picture, at once.
            state.redraw |= state.hidden && !hidden;
            state.hidden = hidden;
        });
    }

    /// The window is gone: the render thread ends.
    pub fn quit(&self) {
        self.with(|state| state.quit = true);
    }

    /// The size of the video drawn, once one was.
    pub fn video_size(&self) -> Option<(u32, u32)> {
        self.state.lock().expect("lock").video
    }

    /// The window lost track of the display drawn (the session came back):
    /// the next picture tells it again.
    pub fn forget_display(&self) {
        self.state.lock().expect("lock").display = None;
    }

    /// (Render thread) Waits for something to do; `None`: the window is gone.
    /// With `retry`, a frame the screen had no room for is drawn again after
    /// that long, if nothing came meanwhile (a still host sends no next one).
    fn next(&self, retry: Option<Duration>) -> Option<Work> {
        let mut state = self.state.lock().expect("lock");
        let until = retry.map(|retry| Instant::now() + retry);
        loop {
            if state.quit {
                return None;
            }
            let picture = state.newest.is_some() && !state.hidden;
            if picture || state.redraw || state.size.is_some() || state.overlay.is_some() {
                let draw = !state.hidden;
                state.redraw = false;
                return Some(Work {
                    frame: if draw { state.newest.take() } else { None },
                    size: state.size.take(),
                    overlay: state.overlay.take(),
                    draw,
                });
            }
            // Hidden, seen again is what draws.
            match until.filter(|_| !state.hidden) {
                Some(until) => {
                    let left = until.saturating_duration_since(Instant::now());
                    state = self.wake.wait_timeout(state, left).expect("lock").0;
                    if Instant::now() >= until {
                        state.redraw = true;
                    }
                }
                None => state = self.wake.wait(state).expect("lock"),
            }
        }
    }

    /// (Render thread) A picture of `display` was uploaded, at `video`;
    /// whether the window must hear of the display.
    fn shown(&self, video: Option<(u32, u32)>, display: u8) -> bool {
        let mut state = self.state.lock().expect("lock");
        state.video = video;
        state.display.replace(display) != Some(display)
    }
}

/// Draws `screen` on a thread of its own, once `pending` has its GPU.
/// `news` goes to the window.
pub fn start(
    name: String,
    screen: Arc<Screen>,
    pending: Pending,
    news: impl Fn(News) + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || draw(&screen, pending, &news))
        .map(drop)
}

fn draw(screen: &Screen, pending: Pending, news: &impl Fn(News)) {
    let mut renderer = match pending.finish() {
        Ok(renderer) => renderer,
        Err(e) => {
            news(News::Failed(e));
            return;
        }
    };
    // Gaps between the frames shown while the picture moves, and since when.
    let (mut gaps, mut since) = (Gaps::default(), Instant::now());
    let mut retry = None;
    while let Some(work) = screen.next(retry) {
        if let Some((width, height)) = work.size {
            renderer.resize(width, height);
        }
        if let Some(overlay) = work.overlay {
            renderer.set_button(overlay.button);
            renderer.set_display_bar(overlay.bar);
            renderer.set_chat_button(overlay.chat);
            renderer.set_handle(overlay.handle);
        }
        if let Some((frame, display)) = work.frame {
            renderer.upload(&frame);
            let now = Instant::now();
            gaps.frame(now);
            if now.duration_since(since) >= Duration::from_secs(1) {
                since = now;
                let (gap, late) = gaps.take();
                debug!(
                    target: hext_net::video::LOG_TARGET,
                    "apresentação: maior intervalo {} ms ({late} acima de {} ms)",
                    gap.as_millis(),
                    Gaps::LATE.as_millis(),
                );
            }
            if screen.shown(renderer.video_size(), display) {
                news(News::Display(display));
            }
        }
        if work.draw {
            let started = Instant::now();
            retry = match renderer.render() {
                Ok(true) => None,
                // On a Mac, just restored: the window may not count as seen
                // yet.
                Ok(false) => Some(RETRY),
                Err(e) => {
                    warn!("render: {e}");
                    None
                }
            };
            let took = started.elapsed();
            if took >= SLOW_RENDER {
                warn!(
                    "quadro preso por {} ms no render (as janelas seguem respondendo)",
                    took.as_millis()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32) -> YuvFrame {
        YuvFrame {
            width,
            height: 2,
            y: vec![0; 4],
            u: vec![0; 1],
            v: vec![0; 1],
        }
    }

    #[test]
    fn a_burst_of_pictures_is_one_frame_drawn() {
        let screen = Screen::default();
        for width in 1..=100 {
            screen.put(picture(width), 1);
        }
        let work = screen.next(None).unwrap();
        let (newest, display) = work.frame.unwrap();
        assert_eq!((newest.width, display), (100, 1));
        assert!(work.draw);
        // Nothing left: the next waits (here, for the end).
        screen.quit();
        assert!(screen.next(None).is_none());
    }

    #[test]
    fn hidden_nothing_is_drawn_and_seen_again_the_newest_is() {
        let screen = Screen::default();
        screen.hide(true);
        screen.put(picture(1), 0);
        screen.put(picture(2), 0);
        // A size still comes through, with nothing to draw.
        screen.resize(800, 600);
        let work = screen.next(None).unwrap();
        assert_eq!(work.size, Some((800, 600)));
        assert!(work.frame.is_none() && !work.draw);
        // Pictures alone do not wake it while hidden.
        screen.put(picture(3), 0);
        {
            let state = screen.state.lock().unwrap();
            assert!(state.newest.is_some() && state.size.is_none() && !state.redraw);
        }
        screen.hide(false);
        let work = screen.next(None).unwrap();
        assert_eq!(work.frame.unwrap().0.width, 3);
        assert!(work.draw);
    }

    #[test]
    fn the_window_hears_of_a_display_once_and_again_when_it_forgets() {
        let screen = Screen::default();
        assert!(screen.shown(Some((4, 2)), 0));
        assert!(!screen.shown(Some((4, 2)), 0));
        assert_eq!(screen.video_size(), Some((4, 2)));
        assert!(screen.shown(Some((4, 2)), 1));
        screen.forget_display();
        assert!(screen.shown(Some((4, 2)), 1));
    }

    #[test]
    fn a_frame_the_screen_had_no_room_for_is_drawn_again() {
        let screen = Screen::default();
        let work = screen.next(Some(Duration::from_millis(10))).unwrap();
        assert!(work.draw && work.frame.is_none());
        // Hidden, it waits to be seen again instead.
        screen.hide(true);
        screen.resize(10, 10);
        let work = screen.next(Some(Duration::from_millis(10))).unwrap();
        assert!(!work.draw);
    }

    #[test]
    fn the_render_thread_wakes_for_a_picture_left_by_another_thread() {
        let screen = Arc::new(Screen::default());
        let decoder = {
            let screen = screen.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                screen.put(picture(7), 2);
            })
        };
        let work = screen.next(None).unwrap();
        assert_eq!(work.frame.unwrap().0.width, 7);
        decoder.join().unwrap();
    }
}
