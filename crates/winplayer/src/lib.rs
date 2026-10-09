//! Windows system video player.
//!
//! The built-in WinRT `MediaPlayer` (hardware decoding, HLS via adaptive
//! streaming, media keys and the Windows media flyout) renders through
//! `Windows.UI.Composition` into a plain Win32 window. No XAML, no WebView.
//!
//! Controls: Space/K or click = pause, ←/→ = 5 s, J/L = 10 s, ↑/↓ = volume,
//! M = mute, F or double-click = fullscreen, Esc = leave fullscreen. A thin bar
//! along the bottom shows progress; click it to seek. Closing the window stops
//! playback and hides it for reuse.
//!
//! [`position`] and [`take_closed`] tell the app how far a video got, so it
//! can resume there.
//!
//! Everything runs on the UI thread that calls [`open`].
#![cfg(windows)]

use std::cell::RefCell;
use std::sync::Once;

use anyhow::{Context as _, Result};
use windows::Foundation::{Size, TimeSpan, TypedEventHandler, Uri};
use windows::Media::Core::MediaSource;
use windows::Media::Playback::{
    IMediaPlaybackSource, MediaPlaybackItem, MediaPlaybackSession, MediaPlaybackState, MediaPlayer,
    MediaPlayerFailedEventArgs, MediaPlayerSurface,
};
use windows::System::{DispatcherQueue, DispatcherQueueController};
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionStretch, Compositor, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, GetMonitorInfoW, GetStockObject, HBRUSH, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Interface, w};
use windows_numerics::{Vector2, Vector3};

const CLASS_NAME: windows::core::PCWSTR = w!("YtLitePlayerWindow");
const TIMER_ID: usize = 1;
/// Height of the clickable seek strip at the bottom, in physical pixels.
const SEEK_STRIP: i32 = 28;

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

struct State {
    hwnd: HWND,
    player: MediaPlayer,
    _compositor: Compositor,
    _target: DesktopWindowTarget,
    _root: ContainerVisual,
    _surface: MediaPlayerSurface,
    progress: SpriteVisual,
    _dispatcher: Option<DispatcherQueueController>,
    title: String,
    /// Saved placement and style while fullscreen.
    fullscreen: Option<(WINDOWPLACEMENT, i32)>,
    /// Where the video was when the window closed, until [`take_closed`].
    closed_at: Option<Times>,
}

/// Playback position and duration, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Times {
    pub position: f64,
    pub duration: Option<f64>,
}

/// Opens `url` (an HLS master playlist or any URL MediaPlayer can open) in the
/// player window, creating it on first use. Playback starts at `start_secs`.
pub fn open(url: &str, title: &str, aspect: Option<f64>, start_secs: f64) -> Result<()> {
    let uri = Uri::CreateUri(&HSTRING::from(url)).context("invalid stream URL")?;
    let media = MediaSource::CreateFromUri(&uri)?;
    let source: IMediaPlaybackSource = if start_secs > 0.0 {
        let start = TimeSpan {
            Duration: (start_secs * 10_000_000.0) as i64,
        };
        MediaPlaybackItem::CreateWithStartTime(&media, start)?.cast()?
    } else {
        media.cast()?
    };

    let reused = STATE.with_borrow_mut(|state| -> Result<bool> {
        let Some(s) = state.as_mut() else {
            return Ok(false);
        };
        s.title = title.to_string();
        s.closed_at = None;
        s.player.SetSource(&source)?;
        s.player.Play()?;
        unsafe {
            let _ = ShowWindow(s.hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(s.hwnd);
        }
        s.update_title();
        Ok(true)
    })?;
    if reused {
        return Ok(());
    }

    let state = create(title, aspect)?;
    state.player.SetSource(&source)?;
    state.player.Play()?;
    let hwnd = state.hwnd;
    STATE.set(Some(state));
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
    with_state(|s| {
        s.resize_surface();
        s.update_title();
    });
    Ok(())
}

/// Where the video in the window is, once it has opened. `None` when the
/// window is closed.
pub fn position() -> Option<Times> {
    with_state(|s| s.playback_times()).flatten()
}

/// Where the video was when the user closed the window (once).
pub fn take_closed() -> Option<Times> {
    with_state(|s| s.closed_at.take()).flatten()
}

fn create(title: &str, aspect: Option<f64>) -> Result<State> {
    let dispatcher = ensure_dispatcher_queue()?;
    let hinstance = unsafe { GetModuleHandleW(None)? };

    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| unsafe {
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // The app icon, if the exe has one (resource 1, like GPUI's windows).
            hIcon: LoadIconW(Some(hinstance.into()), windows::core::PCWSTR(1 as _))
                .unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        RegisterClassExW(&class);
    });

    // 1280 logical pixels wide at the video's aspect ratio.
    let scale = f64::from(unsafe { GetDpiForSystem() }) / 96.0;
    let aspect = aspect
        .filter(|a| a.is_finite() && *a > 0.2 && *a < 5.0)
        .unwrap_or(16.0 / 9.0);
    let (cw, ch) = if aspect >= 1.0 {
        (1280.0, 1280.0 / aspect)
    } else {
        (800.0 * aspect, 800.0)
    };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: (cw * scale) as i32,
        bottom: (ch * scale) as i32,
    };
    unsafe {
        AdjustWindowRectEx(
            &mut rect,
            WS_OVERLAPPEDWINDOW,
            false,
            WS_EX_NOREDIRECTIONBITMAP,
        )?
    };

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            CLASS_NAME,
            &HSTRING::from(title),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(hinstance.into()),
            None,
        )?
    };

    // Visual tree: black background, video (uniformly scaled), progress bar.
    let compositor = Compositor::new()?;
    let interop: ICompositorDesktopInterop = compositor.cast()?;
    let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, false)? };
    let root = compositor.CreateContainerVisual()?;
    root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;

    let background = compositor.CreateSpriteVisual()?;
    background.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
    background.SetBrush(&compositor.CreateColorBrushWithColor(Color {
        A: 255,
        R: 0,
        G: 0,
        B: 0,
    })?)?;

    let player = MediaPlayer::new()?;
    player.SetAutoPlay(true)?;
    player.SetSurfaceSize(Size {
        Width: rect.right as f32,
        Height: rect.bottom as f32,
    })?;
    let surface = player.GetSurface(&compositor)?;
    let brush = compositor.CreateSurfaceBrushWithSurface(&surface.CompositionSurface()?)?;
    brush.SetStretch(CompositionStretch::Uniform)?;
    let video = compositor.CreateSpriteVisual()?;
    video.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
    video.SetBrush(&brush)?;

    // 4 px bar anchored to the bottom edge; width is the playback fraction.
    let bar = |color: Color| -> Result<SpriteVisual> {
        let v = compositor.CreateSpriteVisual()?;
        v.SetBrush(&compositor.CreateColorBrushWithColor(color)?)?;
        v.SetSize(Vector2 { X: 0.0, Y: 4.0 })?;
        v.SetRelativeOffsetAdjustment(Vector3 {
            X: 0.0,
            Y: 1.0,
            Z: 0.0,
        })?;
        v.SetOffset(Vector3 {
            X: 0.0,
            Y: -4.0,
            Z: 0.0,
        })?;
        Ok(v)
    };
    let track = bar(Color {
        A: 70,
        R: 255,
        G: 255,
        B: 255,
    })?;
    track.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 0.0 })?;
    let progress = bar(Color {
        A: 255,
        R: 229,
        G: 9,
        B: 20,
    })?;

    let children = root.Children()?;
    children.InsertAtTop(&background)?;
    children.InsertAtTop(&video)?;
    children.InsertAtTop(&track)?;
    children.InsertAtTop(&progress)?;
    target.SetRoot(&root)?;

    // Surface failures in the title bar; they arrive on a worker thread.
    let hwnd_bits = hwnd.0 as isize;
    player.MediaFailed(
        &TypedEventHandler::<MediaPlayer, MediaPlayerFailedEventArgs>::new(move |_, args| {
            let msg = args
                .as_ref()
                .and_then(|a| a.ErrorMessage().ok())
                .map(|m| m.to_string())
                .unwrap_or_default();
            log::error!("MediaPlayer failed: {msg}");
            let hwnd = HWND(hwnd_bits as *mut _);
            unsafe {
                let _ = SetWindowTextW(hwnd, &HSTRING::from(format!("Playback failed: {msg}")));
            }
            Ok(())
        }),
    )?;

    unsafe { SetTimer(Some(hwnd), TIMER_ID, 500, None) };

    Ok(State {
        hwnd,
        player,
        _compositor: compositor,
        _target: target,
        _root: root,
        _surface: surface,
        progress,
        _dispatcher: dispatcher,
        title: title.to_string(),
        fullscreen: None,
        closed_at: None,
    })
}

/// Windows.UI.Composition needs a DispatcherQueue on this thread.
fn ensure_dispatcher_queue() -> Result<Option<DispatcherQueueController>> {
    if DispatcherQueue::GetForCurrentThread().is_ok() {
        return Ok(None);
    }
    let options = DispatcherQueueOptions {
        dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_NONE,
    };
    let controller = unsafe { CreateDispatcherQueueController(options) }
        .context("creating DispatcherQueue for composition")?;
    Ok(Some(controller))
}

/// Runs `f` on the player state if it exists and isn't already borrowed
/// (window messages can arrive re-entrantly while `open` holds it).
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE
        .try_with(|cell| {
            cell.try_borrow_mut()
                .ok()
                .and_then(|mut s| s.as_mut().map(f))
        })
        .ok()
        .flatten()
}

impl State {
    fn session(&self) -> Option<MediaPlaybackSession> {
        self.player.PlaybackSession().ok()
    }

    fn toggle_pause(&self) {
        let playing = self
            .session()
            .and_then(|s| s.PlaybackState().ok())
            .is_some_and(|st| st == MediaPlaybackState::Playing);
        let _ = if playing {
            self.player.Pause()
        } else {
            self.player.Play()
        };
        self.update_title();
    }

    /// Position and duration in 100 ns ticks.
    fn times(&self) -> Option<(i64, i64)> {
        let s = self.session()?;
        Some((
            s.Position().ok()?.Duration,
            s.NaturalDuration().ok()?.Duration,
        ))
    }

    /// Position and duration once the video has opened (while it opens the
    /// position is 0, which would erase the resume point).
    fn playback_times(&self) -> Option<Times> {
        let state = self.session()?.PlaybackState().ok()?;
        if state == MediaPlaybackState::None || state == MediaPlaybackState::Opening {
            return None;
        }
        let (pos, dur) = self.times()?;
        Some(Times {
            position: pos as f64 / 10_000_000.0,
            duration: (dur > 0).then(|| dur as f64 / 10_000_000.0),
        })
    }

    fn seek_by(&self, secs: i64) {
        let Some((pos, dur)) = self.times() else {
            return;
        };
        let target = (pos + secs * 10_000_000).clamp(0, dur.max(0));
        self.seek_to(target);
    }

    fn seek_to(&self, ticks: i64) {
        if let Some(s) = self.session() {
            let _ = s.SetPosition(TimeSpan { Duration: ticks });
        }
        self.update_progress();
    }

    fn change_volume(&self, delta: f64) {
        if let Ok(v) = self.player.Volume() {
            let _ = self.player.SetVolume((v + delta).clamp(0.0, 1.0));
        }
    }

    fn toggle_mute(&self) {
        if let Ok(m) = self.player.IsMuted() {
            let _ = self.player.SetIsMuted(!m);
        }
        self.update_title();
    }

    fn update_progress(&self) {
        let frac = match self.times() {
            Some((pos, dur)) if dur > 0 => (pos as f64 / dur as f64).clamp(0.0, 1.0) as f32,
            _ => 0.0,
        };
        let _ = self
            .progress
            .SetRelativeSizeAdjustment(Vector2 { X: frac, Y: 0.0 });
    }

    fn update_title(&self) {
        let paused = self
            .session()
            .and_then(|s| s.PlaybackState().ok())
            .is_some_and(|st| st == MediaPlaybackState::Paused);
        let muted = self.player.IsMuted().unwrap_or(false);
        let time = match self.times() {
            Some((pos, dur)) if dur > 0 => format!("{} / {}", clock(pos), clock(dur)),
            Some((pos, _)) => clock(pos),
            None => String::new(),
        };
        let mut text = self.title.clone();
        if !time.is_empty() {
            text.push_str(" — ");
            text.push_str(&time);
        }
        if paused {
            text.push_str(" (paused)");
        }
        if muted {
            text.push_str(" (muted)");
        }
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(text));
        }
    }

    fn resize_surface(&self) {
        let mut rc = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok() && rc.right > 0 && rc.bottom > 0 {
            let _ = self.player.SetSurfaceSize(Size {
                Width: rc.right as f32,
                Height: rc.bottom as f32,
            });
        }
    }

    fn click(&self, x: i32, y: i32) {
        let mut rc = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_err() || rc.right <= 0 {
            return;
        }
        if y >= rc.bottom - SEEK_STRIP {
            if let Some((_, dur)) = self.times().filter(|(_, d)| *d > 0) {
                let frac = (f64::from(x) / f64::from(rc.right)).clamp(0.0, 1.0);
                self.seek_to((dur as f64 * frac) as i64);
            }
        } else {
            self.toggle_pause();
        }
    }

    /// Borderless window covering the monitor, and back.
    fn toggle_fullscreen(&mut self) {
        unsafe {
            if let Some((placement, style)) = self.fullscreen.take() {
                SetWindowLongW(self.hwnd, GWL_STYLE, style);
                let _ = SetWindowPlacement(self.hwnd, &placement);
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
                );
                return;
            }
            let style = GetWindowLongW(self.hwnd, GWL_STYLE);
            let mut placement = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            let mut monitor = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetWindowPlacement(self.hwnd, &mut placement).is_err()
                || !GetMonitorInfoW(
                    MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST),
                    &mut monitor,
                )
                .as_bool()
            {
                return;
            }
            SetWindowLongW(
                self.hwnd,
                GWL_STYLE,
                style & !(WS_OVERLAPPEDWINDOW.0 as i32),
            );
            let r = monitor.rcMonitor;
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOP),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
            );
            self.fullscreen = Some((placement, style));
        }
    }

    /// Stops playback, releases the stream, hides the window for reuse.
    fn close(&mut self) {
        if self.fullscreen.is_some() {
            self.toggle_fullscreen();
        }
        self.closed_at = self.playback_times();
        let _ = self.player.Pause();
        let _ = self.player.SetSource(None::<&IMediaPlaybackSource>);
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        log::info!("player window closed");
    }

    fn key(&mut self, vk: u16) {
        match vk {
            k if k == VK_SPACE.0 || k == u16::from(b'K') => self.toggle_pause(),
            k if k == VK_LEFT.0 => self.seek_by(-5),
            k if k == VK_RIGHT.0 => self.seek_by(5),
            k if k == u16::from(b'J') => self.seek_by(-10),
            k if k == u16::from(b'L') => self.seek_by(10),
            k if k == VK_UP.0 => self.change_volume(0.1),
            k if k == VK_DOWN.0 => self.change_volume(-0.1),
            k if k == u16::from(b'M') => self.toggle_mute(),
            k if k == u16::from(b'F') => self.toggle_fullscreen(),
            k if k == VK_ESCAPE.0 && self.fullscreen.is_some() => self.toggle_fullscreen(),
            _ => {}
        }
    }
}

fn clock(ticks: i64) -> String {
    let secs = (ticks / 10_000_000).max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn loword(v: isize) -> i32 {
    (v & 0xffff) as i16 as i32
}

fn hiword(v: isize) -> i32 {
    ((v >> 16) & 0xffff) as i16 as i32
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let handled = match msg {
        WM_CLOSE => with_state(|s| s.close()).is_some(),
        WM_SIZE => with_state(|s| s.resize_surface()).is_some(),
        WM_KEYDOWN => with_state(|s| s.key(wparam.0 as u16)).is_some(),
        WM_LBUTTONUP => with_state(|s| s.click(loword(lparam.0), hiword(lparam.0))).is_some(),
        WM_LBUTTONDBLCLK => with_state(|s| s.toggle_fullscreen()).is_some(),
        WM_TIMER if wparam.0 == TIMER_ID => with_state(|s| {
            s.update_progress();
            s.update_title();
        })
        .is_some(),
        _ => false,
    };
    // Never destroy the window on close: it is hidden and reused.
    if handled || msg == WM_CLOSE {
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_format() {
        assert_eq!(clock(45 * 10_000_000), "0:45");
        assert_eq!(clock(3723 * 10_000_000), "1:02:03");
    }

    #[test]
    fn lparam_words() {
        let v = (200 << 16) | 100;
        assert_eq!((loword(v), hiword(v)), (100, 200));
    }
}
