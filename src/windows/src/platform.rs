//! Windows platform impl: window init/cleanup, fullscreen toggle
//! helpers, scale + geometry queries, and the WndProc hook that drives
//! compositor resize + transition bookkeeping.
//!
//! All `g_win` state (HWND, cached scale, fullscreen bookkeeping, the
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
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    CWPSTRUCT, CallNextHookEx, GWL_STYLE, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, HHOOK, IsIconic, IsZoomed, SIZE_MINIMIZED, SPI_GETWORKAREA,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowsHookExW, SystemParametersInfoW,
    UnhookWindowsHookEx, WH_CALLWNDPROC, WM_CLOSE, WM_SETFOCUS, WM_SIZE, WS_CAPTION, WS_THICKFRAME,
};

use jfn_mpv::api::{
    jfn_mpv_get_property_int, jfn_mpv_set_fullscreen, jfn_mpv_set_window_maximized,
    jfn_mpv_set_window_minimized, jfn_mpv_toggle_fullscreen,
};
use jfn_mpv::boot::jfn_mpv_handle_get;
use jfn_platform_abi::{
    WindowExtent,
    geometry::{Bounds, WindowGeometry, clamp_to_bounds},
};
use jfn_playback::ingest_driver::{jfn_playback_display_scale, jfn_playback_fullscreen};
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
    cached_scale: f32,

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
            cached_scale: 1.0,
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
    let scale = jfn_playback_display_scale();
    if scale > 0.0 {
        let s = scale as f32;
        STATE.lock().cached_scale = s;
        return s;
    }
    let cached = STATE.lock().cached_scale;
    if cached > 0.0 {
        return cached;
    }
    // Pre-mpv (default-geometry sizing at startup): ask the OS directly.
    let dpi = unsafe { GetDpiForSystem() };
    if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 }
}

// Per-monitor DPI (GetDpiForMonitor) lives in Shcore.dll which isn't
// currently linked; fall back to system DPI and ignore (x, y).
pub fn win_get_display_scale(_x: c_int, _y: c_int) -> f32 {
    let dpi = unsafe { GetDpiForSystem() };
    if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 }
}

/// Keep Win32 input in the same coordinate space as CEF. Both consumers read
/// the mpv-owned `WindowExtent`; this callback is needed because a display
/// scale change can update that extent without producing a `WM_SIZE`.
fn sync_input_geometry_from_mpv_extent() {
    let Some(extent) = jfn_playback::ingest_driver::jfn_playback_window_extent() else {
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
        "synchronizing Windows input geometry from mpv WindowExtent"
    );
    jfn_input_windows_resize_to_parent(logical_w, logical_h, physical_w, physical_h);
}

fn input_geometry_for_parent_size(
    physical_w: c_int,
    physical_h: c_int,
) -> Option<(c_int, c_int, c_int, c_int)> {
    let extent = jfn_playback::ingest_driver::jfn_playback_window_extent()?;
    let geometry = input_geometry_from_extent(extent)?;
    input_geometry_matches_parent(geometry, physical_w, physical_h).then_some(geometry)
}

fn input_geometry_from_extent(extent: WindowExtent) -> Option<(c_int, c_int, c_int, c_int)> {
    let logical = extent.logical();
    let physical = extent.physical();
    (logical.w > 0 && logical.h > 0 && physical.w > 0 && physical.h > 0)
        .then_some((logical.w, logical.h, physical.w, physical.h))
}

fn input_geometry_matches_parent(
    geometry: (c_int, c_int, c_int, c_int),
    physical_w: c_int,
    physical_h: c_int,
) -> bool {
    geometry.2 == physical_w && geometry.3 == physical_h
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

                let lparam = msg.lParam.0 as u32;
                let pw = (lparam & 0xFFFF) as c_int;
                let ph = ((lparam >> 16) & 0xFFFF) as c_int;
                if pw > 0 && ph > 0 {
                    // Prefer the same exact extent CEF receives. WM_SIZE may
                    // race mpv's OSD update, so only use it when the physical
                    // dimensions match and otherwise retain the old fallback.
                    let (lw, lh) = input_geometry_for_parent_size(pw, ph)
                        .map(|(lw, lh, _, _)| (lw, lh))
                        .unwrap_or_else(|| {
                            let cached = STATE.lock().cached_scale;
                            let scale = if cached > 0.0 { cached } else { 1.0 };
                            (
                                (pw as f32 / scale).round() as c_int,
                                (ph as f32 / scale).round() as c_int,
                            )
                        });
                    jfn_input_windows_resize_to_parent(lw, lh, pw, ph);

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
                    crate::compositor::jfn_win_update_surface_size(
                        lw,
                        lh,
                        pw,
                        ph,
                        fs_changed || recovering_from_minimize,
                    );
                }
            } else if msg.message == WM_SETFOCUS {
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

    // Seed cached_scale.
    win_get_scale();

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
    jfn_platform_abi::subscribe_window_changed(sync_input_geometry_from_mpv_extent);
    sync_input_geometry_from_mpv_extent();

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
    use super::{input_geometry_from_extent, input_geometry_matches_parent};
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
        let old = WindowExtent::with_logical(
            PhysicalSize { w: 1311, h: 736 },
            Scale(1.25),
            LogicalSize { w: 1049, h: 589 },
        );
        let new = WindowExtent::with_logical(
            PhysicalSize { w: 1311, h: 736 },
            Scale(1.5),
            LogicalSize { w: 874, h: 491 },
        );

        assert_eq!(
            input_geometry_from_extent(old),
            Some((1049, 589, 1311, 736))
        );
        assert_eq!(input_geometry_from_extent(new), Some((874, 491, 1311, 736)));
    }

    #[test]
    fn parent_resize_rejects_a_stale_mpv_extent() {
        let matching = WindowExtent::with_logical(
            PhysicalSize { w: 1311, h: 736 },
            Scale(1.25),
            LogicalSize { w: 1048, h: 589 },
        );
        let different_size = WindowExtent::with_logical(
            PhysicalSize { w: 1600, h: 900 },
            Scale(1.25),
            LogicalSize { w: 1280, h: 720 },
        );

        assert_eq!(
            input_geometry_from_extent(matching),
            Some((1048, 589, 1311, 736))
        );
        assert_eq!(
            input_geometry_from_extent(different_size),
            Some((1280, 720, 1600, 900))
        );
        assert!(input_geometry_matches_parent(
            (1048, 589, 1311, 736),
            1311,
            736
        ));
        assert!(!input_geometry_matches_parent(
            (1280, 720, 1600, 900),
            1311,
            736
        ));
    }
}
