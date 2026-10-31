//! Windows platform impl: window init/cleanup, fullscreen toggle
//! helpers, scale + geometry queries, and the WndProc hook that drives
//! compositor resize + transition bookkeeping.
//!
//! All `g_win` state (HWND, fullscreen bookkeeping, the
//! WndProc hook handle, the input thread JoinHandle) lives in this
//! module behind a `Mutex<WinState>`.

#![allow(non_snake_case)]

use parking_lot::Mutex;
use std::ffi::{c_int, c_void};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
    ProcessPowerThrottling, SetProcessInformation,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    CWPSTRUCT, CallNextHookEx, GWL_STYLE, GetClientRect, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, HHOOK, IsIconic, IsZoomed, SIZE_MINIMIZED, SPI_GETWORKAREA,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowsHookExW, SystemParametersInfoW,
    UnhookWindowsHookEx, WH_CALLWNDPROC, WM_CLOSE, WM_DPICHANGED, WM_SETFOCUS, WM_SIZE,
    WM_WINDOWPOSCHANGED, WS_CAPTION, WS_THICKFRAME,
};

use jfn_mpv::api::{
    jfn_mpv_get_property_int, jfn_mpv_set_fullscreen, jfn_mpv_set_window_maximized,
    jfn_mpv_set_window_minimized, jfn_mpv_toggle_fullscreen,
};
use jfn_mpv::boot::jfn_mpv_handle_get;
use jfn_platform_abi::{
    PhysicalSize, Scale, WindowExtent, WindowPos, WindowSnapshot, WindowSource,
    geometry::{Bounds, WindowGeometry, clamp_to_bounds},
};
use jfn_playback::ingest_driver::jfn_playback_fullscreen;
use jfn_playback::shutdown::jfn_shutdown_initiate;

// Input thread lives in `crate::input`.
use crate::input::{
    jfn_input_windows_focus, jfn_input_windows_resize_to_parent,
    jfn_input_windows_run_input_thread, jfn_input_windows_stop_input_thread,
};

// =====================================================================
// File-static state — equivalent of the C++ `static WinState g_win`.
// All access is serialized through `STATE.lock()`. HWND / HHOOK are raw
// pointer types; we are the sole writer so the Mutex is sufficient.
// =====================================================================

struct WinState {
    mpv_hwnd_raw: usize,

    // Fullscreen-transition bookkeeping read/written by the WndProc and
    // the fullscreen toggle helpers.
    was_fullscreen: bool,
    was_minimized: bool,
    restore_maximized_on_unfullscreen: bool,

    wndproc_hook_raw: usize,
    input_thread: Option<JoinHandle<()>>,
}

impl WinState {
    const fn new() -> Self {
        Self {
            mpv_hwnd_raw: 0,
            was_fullscreen: false,
            was_minimized: false,
            restore_maximized_on_unfullscreen: false,
            wndproc_hook_raw: 0,
            input_thread: None,
        }
    }
}

static STATE: Mutex<WinState> = Mutex::new(WinState::new());

fn hwnd_from_raw(raw: usize) -> HWND {
    HWND(raw as *mut c_void)
}

// =====================================================================
// Narrow accessors.
// =====================================================================

/// `jfn_win_get_hwnd` — returns mpv's HWND; nullptr before win_init or
/// after cleanup.
pub fn jfn_win_get_hwnd() -> *mut c_void {
    STATE.lock().mpv_hwnd_raw as *mut c_void
}

fn is_fullscreen_style(style: isize) -> bool {
    let s = style as u32;
    (s & WS_CAPTION.0) == 0 && (s & WS_THICKFRAME.0) == 0
}

// =====================================================================
// Scale + content-size lookups.
// =====================================================================

pub fn win_get_scale() -> f32 {
    let hwnd_raw = STATE.lock().mpv_hwnd_raw;
    let dpi = if hwnd_raw == 0 {
        // Before mpv creates the window, boot geometry uses the system DPI.
        unsafe { GetDpiForSystem() }
    } else {
        unsafe { GetDpiForWindow(hwnd_from_raw(hwnd_raw)) }
    };
    if dpi == 0 {
        tracing::error!(hwnd_raw, "Windows window DPI is unavailable");
    }
    dpi as f32 / 96.0
}

// Per-monitor DPI (GetDpiForMonitor) lives in Shcore.dll which isn't
// currently linked; fall back to system DPI and ignore (x, y).
pub fn win_get_display_scale(_x: c_int, _y: c_int) -> f32 {
    let dpi = unsafe { GetDpiForSystem() };
    if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 }
}

/// The native client rect and window DPI are authoritative on Windows. mpv's
/// OSD properties can lag behind them while the window is idle or minimized.
pub(crate) struct WindowsWindowSource;

pub(crate) static WINDOWS_WINDOW_SOURCE: WindowsWindowSource = WindowsWindowSource;

fn window_source_hwnd() -> HWND {
    let hwnd_raw = STATE.lock().mpv_hwnd_raw;
    if hwnd_raw != 0 || jfn_mpv_handle_get().is_null() {
        return hwnd_from_raw(hwnd_raw);
    }
    // Startup waits for a usable WindowSource before calling win_init. Only
    // obtain the native handle from mpv here; dimensions still come from Win32.
    let mut wid = 0;
    if unsafe { jfn_mpv_get_property_int(c"window-id".as_ptr(), &mut wid) } < 0 {
        return HWND::default();
    }
    hwnd_from_raw(wid as usize)
}

impl WindowSource for WindowsWindowSource {
    fn snapshot(&self) -> WindowSnapshot {
        let hwnd = window_source_hwnd();
        let (mut x, mut y) = (0, 0);
        WindowSnapshot {
            extent: native_window_extent(hwnd),
            position: win_query_window_position(&mut x, &mut y).then_some(WindowPos { x, y }),
            maximized: !hwnd.0.is_null() && unsafe { IsZoomed(hwnd) }.as_bool(),
            fullscreen: !hwnd.0.is_null()
                && is_fullscreen_style(unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) }),
        }
    }
}

fn extent_from_client_geometry(width: c_int, height: c_int, dpi: u32) -> Option<WindowExtent> {
    (width > 0 && height > 0 && dpi > 0).then(|| {
        WindowExtent::new(
            PhysicalSize {
                w: width,
                h: height,
            },
            Scale(dpi as f32 / 96.0),
        )
    })
}

fn native_window_extent(hwnd: HWND) -> Option<WindowExtent> {
    if hwnd.0.is_null() || unsafe { IsIconic(hwnd) }.as_bool() {
        return None;
    }
    let mut rect = RECT::default();
    if let Err(error) = unsafe { GetClientRect(hwnd, &mut rect) } {
        tracing::warn!(?error, "Cannot read Windows client geometry");
        return None;
    }
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        tracing::warn!("Cannot read Windows client DPI");
        return None;
    }
    extent_from_client_geometry(rect.right - rect.left, rect.bottom - rect.top, dpi)
}

/// Keep input and CEF on the same native source, including scale-only changes.
fn sync_input_geometry_from_window() {
    let hwnd = hwnd_from_raw(STATE.lock().mpv_hwnd_raw);
    let Some(extent) = native_window_extent(hwnd) else {
        return;
    };
    let Some((logical_w, logical_h, physical_w, physical_h)) = input_geometry_from_extent(extent)
    else {
        return;
    };
    tracing::debug!(
        logical_w,
        logical_h,
        physical_w,
        physical_h,
        "synchronizing Windows input geometry from native client rect and DPI"
    );
    jfn_input_windows_resize_to_parent(logical_w, logical_h, physical_w, physical_h);
}

fn input_geometry_from_extent(extent: WindowExtent) -> Option<(c_int, c_int, c_int, c_int)> {
    let logical = extent.logical();
    let physical = extent.physical();
    (logical.w > 0 && logical.h > 0 && physical.w > 0 && physical.h > 0)
        .then_some((logical.w, logical.h, physical.w, physical.h))
}

fn sync_native_window_geometry(force_end: bool) {
    let hwnd = hwnd_from_raw(STATE.lock().mpv_hwnd_raw);
    let Some(extent) = native_window_extent(hwnd) else {
        return;
    };
    let logical = extent.logical();
    let physical = extent.physical();
    crate::compositor::jfn_win_update_surface_size(
        logical.w, logical.h, physical.w, physical.h, force_end,
    );
    // Notify only after native state is committed. Late mpv notifications also
    // pull this same source, so stale OSD dimensions cannot shrink the browser.
    jfn_platform_abi::notify_window_changed();
}

// =====================================================================
// Fullscreen toggle helpers.
// =====================================================================

fn end_transition_if_settled(target_fullscreen: bool) {
    if crate::win_in_transition() {
        let was_fs = STATE.lock().was_fullscreen;
        if target_fullscreen == was_fs {
            crate::compositor::jfn_win_wndproc_end_transition_locked();
        }
    }
}

pub fn win_set_fullscreen(fullscreen: bool) {
    if jfn_mpv_handle_get().is_null() {
        return;
    }
    if jfn_playback_fullscreen() == fullscreen {
        end_transition_if_settled(fullscreen);
        return;
    }

    let hwnd_raw = STATE.lock().mpv_hwnd_raw;
    let hwnd = hwnd_from_raw(hwnd_raw);

    if fullscreen {
        STATE.lock().restore_maximized_on_unfullscreen = unsafe { IsZoomed(hwnd) }.as_bool();
    }

    let mut should_restore_maximized = false;
    if !fullscreen {
        let mut st = STATE.lock();
        should_restore_maximized = st.restore_maximized_on_unfullscreen;
        st.restore_maximized_on_unfullscreen = false;
    }

    let is_minimized_now = unsafe { IsIconic(hwnd) }.as_bool();
    if !is_minimized_now {
        crate::compositor::jfn_win_wndproc_begin_transition_locked();
    }

    if fullscreen {
        jfn_mpv_set_window_minimized(false);
    }

    jfn_mpv_set_fullscreen(fullscreen);

    if !fullscreen && should_restore_maximized {
        jfn_mpv_set_window_maximized(true);
    }
}

pub fn win_toggle_fullscreen() {
    if jfn_mpv_handle_get().is_null() {
        return;
    }
    let target_fullscreen = !jfn_playback_fullscreen();

    let hwnd_raw = STATE.lock().mpv_hwnd_raw;
    let hwnd = hwnd_from_raw(hwnd_raw);

    if target_fullscreen {
        STATE.lock().restore_maximized_on_unfullscreen = unsafe { IsZoomed(hwnd) }.as_bool();
    }

    let mut should_restore_maximized = false;
    if !target_fullscreen {
        let mut st = STATE.lock();
        should_restore_maximized = st.restore_maximized_on_unfullscreen;
        st.restore_maximized_on_unfullscreen = false;
    }

    let is_minimized_now = unsafe { IsIconic(hwnd) }.as_bool();
    if !is_minimized_now {
        crate::compositor::jfn_win_wndproc_begin_transition_locked();
    }

    if target_fullscreen {
        jfn_mpv_set_window_minimized(false);
    }

    jfn_mpv_toggle_fullscreen();

    if !target_fullscreen && should_restore_maximized {
        jfn_mpv_set_window_maximized(true);
    }
}

// =====================================================================
// WndProc hook.
// =====================================================================

unsafe extern "system" fn mpv_wndproc_hook(n_code: c_int, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if n_code >= 0 {
        let msg = unsafe { &*(lp.0 as *const CWPSTRUCT) };
        let target_hwnd_raw = STATE.lock().mpv_hwnd_raw;
        if (msg.hwnd.0 as usize) == target_hwnd_raw {
            if msg.message == WM_SIZE {
                if msg.wParam.0 == SIZE_MINIMIZED as usize {
                    let was_minimized = STATE.lock().was_minimized;
                    STATE.lock().was_minimized = true;
                    if !was_minimized {
                        jfn_playback::lifecycle::jfn_lifecycle_set_visible(false);
                    }
                    let hook_raw = STATE.lock().wndproc_hook_raw;
                    let hook = HHOOK(hook_raw as *mut c_void);
                    return unsafe { CallNextHookEx(Some(hook), n_code, wp, lp) };
                }

                if let Some(extent) = native_window_extent(hwnd_from_raw(target_hwnd_raw)) {
                    let style =
                        unsafe { GetWindowLongPtrW(hwnd_from_raw(target_hwnd_raw), GWL_STYLE) };
                    let fs = is_fullscreen_style(style);

                    // Fullscreen-style edge, computed before mutating stored
                    // state so we can both decide whether to *begin* a
                    // transition and tell the compositor when to *end* one.
                    let was_fs = STATE.lock().was_fullscreen;
                    let fs_changed = fs != was_fs;
                    let recovering_from_minimize = STATE.lock().was_minimized;

                    if recovering_from_minimize {
                        let mut st = STATE.lock();
                        st.was_minimized = false;
                        st.was_fullscreen = fs;
                        drop(st);
                        // Restore from iconic — counterpart to the
                        // SIZE_MINIMIZED arm above.
                        jfn_playback::lifecycle::jfn_lifecycle_set_visible(true);
                        tracing::info!(
                            physical_w = extent.physical().w,
                            physical_h = extent.physical().h,
                            scale = extent.scale().0,
                            "Windows window restored; synchronizing native geometry"
                        );
                    } else if fs_changed {
                        STATE.lock().was_fullscreen = fs;
                        // A fullscreen change we didn't drive through the
                        // toggle helpers (e.g. an mpv-initiated one) needs a
                        // transition begun here so the stale-size OSD is
                        // detached until the window settles. Helper-initiated
                        // changes already began one; never start a second.
                        // Starting a second is what previously left
                        // G_TRANSITIONING stuck across multiple WM_SIZE events
                        // and blanked the OSD on exit from fullscreen.
                        if !crate::win_in_transition() {
                            crate::compositor::jfn_win_wndproc_begin_transition_locked();
                        }
                    }

                    // End any in-progress transition once the window has
                    // actually reached its new physical size (handled inside
                    // the compositor, where the size captured at begin lives).
                    // The force-end flag covers a settled fullscreen edge whose
                    // physical size happens to be unchanged.
                    sync_native_window_geometry(fs_changed || recovering_from_minimize);
                }
            } else if msg.message == WM_DPICHANGED || msg.message == WM_WINDOWPOSCHANGED {
                // WH_CALLWNDPROC runs before mpv's handler. DPI changes may
                // resize the window in that handler; the subsequent position /
                // size notifications then pull the final client rect again.
                if msg.message == WM_DPICHANGED {
                    tracing::info!(dpi = msg.wParam.0 & 0xffff, "Windows window DPI changed");
                }
                sync_native_window_geometry(false);
            } else if msg.message == WM_SETFOCUS {
                sync_native_window_geometry(false);
                jfn_input_windows_focus();
            } else if msg.message == WM_CLOSE {
                tracing::info!("Windows window close received; initiating application shutdown");
                jfn_shutdown_initiate();
            }
        }
    }
    let hook_raw = STATE.lock().wndproc_hook_raw;
    let hook = HHOOK(hook_raw as *mut c_void);
    unsafe { CallNextHookEx(Some(hook), n_code, wp, lp) }
}

// =====================================================================
// Platform vtable entry points.
// =====================================================================

pub fn win_early_init() {
    // Windows 11 may ignore a fully occluded process's high-resolution timer
    // requests. CEF's OSR BeginFrame source depends on those timers, and the
    // policy otherwise persists for several seconds after the window returns.
    // `early_init` runs before CEF subprocess dispatch, so every Chromium
    // process applies the policy to itself.
    let power_throttling = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    let result = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            std::ptr::from_ref(&power_throttling).cast(),
            std::mem::size_of_val(&power_throttling) as u32,
        )
    };
    if let Err(error) = result {
        eprintln!("[windows] failed to preserve timer resolution while occluded: {error:?}");
    }
}

pub fn win_init(_mpv: *mut c_void) -> bool {
    let mut wid: i64 = 0;
    let name = c"window-id";
    let rc = unsafe { jfn_mpv_get_property_int(name.as_ptr(), &mut wid) };
    if rc < 0 || wid == 0 {
        tracing::error!("Failed to get window-id from mpv");
        return false;
    }
    let hwnd_raw = wid as usize;
    STATE.lock().mpv_hwnd_raw = hwnd_raw;

    // Enable DWM transparency so DComp visuals with premultiplied alpha work.
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd_from_raw(hwnd_raw), &margins);
    }

    if !crate::compositor::jfn_win_init_compositor(hwnd_raw as *mut c_void) {
        return false;
    }

    // Seed was_fullscreen before installing the hook so the first WM_SIZE
    // doesn't start a spurious transition if already fullscreen.
    {
        let style = unsafe { GetWindowLongPtrW(hwnd_from_raw(hwnd_raw), GWL_STYLE) };
        STATE.lock().was_fullscreen = is_fullscreen_style(style);
    }

    let mpv_tid = unsafe { GetWindowThreadProcessId(hwnd_from_raw(hwnd_raw), None) };
    // Observe WM_CLOSE before mpv's WndProc consumes it as CLOSE_WIN. A
    // WH_CALLWNDPROCRET hook runs too late for that path and can leave the
    // browser process plus its CEF helpers alive after the visible window is
    // closed.
    let hook = unsafe { SetWindowsHookExW(WH_CALLWNDPROC, Some(mpv_wndproc_hook), None, mpv_tid) };
    match hook {
        Ok(h) => STATE.lock().wndproc_hook_raw = h.0 as usize,
        Err(e) => {
            tracing::error!("SetWindowsHookExW(WH_CALLWNDPROC) failed: {e:?}");
            return false;
        }
    }

    let mpv_hwnd_for_thread = hwnd_raw;
    let join = std::thread::spawn(move || {
        jfn_input_windows_run_input_thread(mpv_hwnd_for_thread as *mut c_void);
    });
    STATE.lock().input_thread = Some(join);

    // CEF already resizes from this payload-free notification. Subscribe the
    // input layer to the same source so a scale-only change cannot leave its
    // physical-to-logical mapping behind CEF's view size.
    jfn_platform_abi::subscribe_window_changed(sync_input_geometry_from_window);
    sync_native_window_geometry(false);

    tracing::info!("Windows DirectComposition compositor initialized");
    true
}

pub fn win_cleanup() {
    jfn_input_windows_stop_input_thread();
    let join = STATE.lock().input_thread.take();
    if let Some(j) = join {
        let _ = j.join();
    }
    let hook_raw = STATE.lock().wndproc_hook_raw;
    if hook_raw != 0 {
        let hook = HHOOK(hook_raw as *mut c_void);
        unsafe {
            let _ = UnhookWindowsHookEx(hook);
        }
        STATE.lock().wndproc_hook_raw = 0;
    }

    crate::compositor::jfn_win_cleanup_compositor();

    STATE.lock().mpv_hwnd_raw = 0;
}

// =====================================================================
// Window-position / geometry helpers.
// =====================================================================

/// Query window position relative to the monitor's working area (excludes
/// taskbar), in physical pixels. Matches mpv's `--geometry +X+Y`
/// coordinate system on Windows (`vo_calc_window_geometry` uses the
/// working area).
pub fn win_query_window_position(x: &mut c_int, y: &mut c_int) -> bool {
    let hwnd_raw = STATE.lock().mpv_hwnd_raw;
    if hwnd_raw == 0 {
        return false;
    }
    let hwnd = hwnd_from_raw(hwnd_raw);
    let mut wr = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut wr) }.is_err() {
        return false;
    }
    let mon: HMONITOR = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        return false;
    }
    *x = wr.left - mi.rcWork.left;
    *y = wr.top - mi.rcWork.top;
    true
}

/// Resolve saved geometry against the primary monitor's working area so the
/// window never opens larger than the screen or off-screen, and center any
/// unset axis.
pub fn win_clamp_window_geometry(w: &mut c_int, h: &mut c_int, x: &mut c_int, y: &mut c_int) {
    let mut work = RECT::default();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut RECT as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if ok.is_err() {
        return;
    }
    let vw = work.right - work.left;
    let vh = work.bottom - work.top;
    let mut g = WindowGeometry::from_raw(*w, *h, *x, *y);
    clamp_to_bounds(&mut g, Bounds { w: vw, h: vh });
    *w = g.w;
    *h = g.h;
    let (nx, ny) = g.raw_position();
    *x = nx;
    *y = ny;
}

#[cfg(test)]
mod tests {
    use super::{extent_from_client_geometry, input_geometry_from_extent};
    use jfn_platform_abi::{LogicalSize, PhysicalSize, Scale, WindowExtent};

    #[test]
    fn input_geometry_uses_the_exact_cef_window_extent() {
        let extent = WindowExtent::with_logical(
            PhysicalSize { w: 1311, h: 736 },
            Scale(1.25),
            LogicalSize { w: 1049, h: 589 },
        );

        assert_eq!(
            input_geometry_from_extent(extent),
            Some((1049, 589, 1311, 736))
        );
    }

    #[test]
    fn scale_only_extent_change_updates_input_logical_size() {
        assert_eq!(
            extent_from_client_geometry(1311, 736, 120).and_then(input_geometry_from_extent),
            Some((1049, 589, 1311, 736))
        );
        assert_eq!(
            extent_from_client_geometry(1311, 736, 144).and_then(input_geometry_from_extent),
            Some((874, 491, 1311, 736))
        );
    }

    #[test]
    fn background_dpi_resize_preserves_logical_size() {
        // Captured failure: startup was 1515x955 at 125%, while the live
        // client rect had become 1818x1146 at 150% with no user resize.
        assert_eq!(
            extent_from_client_geometry(1515, 955, 120).and_then(input_geometry_from_extent),
            Some((1212, 764, 1515, 955))
        );
        assert_eq!(
            extent_from_client_geometry(1818, 1146, 144).and_then(input_geometry_from_extent),
            Some((1212, 764, 1818, 1146))
        );
    }

    #[test]
    fn invalid_native_geometry_is_not_replaced_with_a_cached_size() {
        for (width, height, dpi) in [
            (0, 720, 144),
            (1280, 0, 144),
            (-1, 720, 144),
            (1280, 720, 0),
        ] {
            assert!(extent_from_client_geometry(width, height, dpi).is_none());
        }
    }
}
