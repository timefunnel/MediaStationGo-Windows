//! Windows input — Win32 child window owning all keyboard/mouse for CEF.
//!
//! Runs on a dedicated thread (spawned by `platform.rs::win_init`);
//! registers a `JellyfinCefInput` window class, creates a child of mpv's
//! HWND covering the client area, and translates `WM_*` messages into
//! the platform-agnostic `jfn_input_dispatch_*` entry points exposed by
//! `src/input/src/lib.rs`.

#![allow(non_snake_case)]

use parking_lot::Mutex;
use std::ffi::c_int;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::{
    APPCOMMAND_BROWSER_BACKWARD, APPCOMMAND_BROWSER_FORWARD, LANG_CHINESE, LANG_JAPANESE,
    LANG_KOREAN, MK_CONTROL, MK_LBUTTON, MK_MBUTTON, MK_RBUTTON, MK_SHIFT,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::Ime::{
    ATTR_TARGET_CONVERTED, ATTR_TARGET_NOTCONVERTED, CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_EXCLUDE,
    CS_NOMOVECARET, GCS_COMPATTR, GCS_COMPCLAUSE, GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR, HIMC,
    ISC_SHOWUICOMPOSITIONWINDOW, ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext,
    ImmSetCandidateWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, GetKeyboardLayoutNameW, SetFocus, VK_ADD, VK_BROWSER_BACK,
    VK_BROWSER_FORWARD, VK_CAPITAL, VK_CLEAR, VK_CONTROL, VK_DECIMAL, VK_DELETE, VK_DIVIDE,
    VK_DOWN, VK_END, VK_F4, VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_MENU, VK_MULTIPLY, VK_NEXT, VK_NUMLOCK, VK_NUMPAD0, VK_NUMPAD1, VK_NUMPAD2, VK_NUMPAD3,
    VK_NUMPAD4, VK_NUMPAD5, VK_NUMPAD6, VK_NUMPAD7, VK_NUMPAD8, VK_NUMPAD9, VK_PRIOR, VK_RCONTROL,
    VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_SUBTRACT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateCaret, CreateWindowExW, DefWindowProcW, DestroyCaret, DestroyWindow, DispatchMessageW,
    GetClientRect, GetMessageW, GetWindowThreadProcessId, HCURSOR, HICON, HMENU, HTCLIENT,
    IDC_APPSTARTING, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_HELP, IDC_IBEAM, IDC_NO, IDC_SIZEALL,
    IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT, KF_EXTENDED, LoadCursorW, MSG,
    PostMessageW, PostThreadMessageW, RegisterClassExW, SET_WINDOW_POS_FLAGS, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOZORDER, SetCaretPos, SetCursor, SetWindowPos, TranslateMessage,
    UnregisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APPCOMMAND, WM_CHAR, WM_IME_COMPOSITION,
    WM_IME_ENDCOMPOSITION, WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDBLCLK,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS, WM_SYSCHAR, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSEXW, WS_CHILD, WS_VISIBLE, XBUTTON2,
};
use windows::core::{PCWSTR, w};

// Not re-exported by windows-rs 0.62's WindowsAndMessaging metadata.
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_APP_FOCUS_INPUT: u32 = 0x8000 + 1;
const WM_APP_IME_POSITION: u32 = 0x8000 + 2;

// =====================================================================
// CEF cursor-type ordinals + event flags (mirrors cef_types.h)
// =====================================================================

use jfn_input::buttons::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
use jfn_platform_abi::cursor::CursorShape;
use jfn_platform_abi::event_flags::{
    EVENTFLAG_ALT_DOWN, EVENTFLAG_CAPS_LOCK_ON, EVENTFLAG_CONTROL_DOWN, EVENTFLAG_IS_KEY_PAD,
    EVENTFLAG_IS_LEFT, EVENTFLAG_IS_RIGHT, EVENTFLAG_LEFT_MOUSE_BUTTON,
    EVENTFLAG_MIDDLE_MOUSE_BUTTON, EVENTFLAG_NUM_LOCK_ON, EVENTFLAG_RIGHT_MOUSE_BUTTON,
    EVENTFLAG_SHIFT_DOWN,
};
use jfn_platform_abi::{ImeTextRange, ImeUnderline, JfnRect};

use jfn_input::{
    jfn_input_dispatch_char_sys, jfn_input_dispatch_history_nav,
    jfn_input_dispatch_ime_cancel_composition, jfn_input_dispatch_ime_commit_text,
    jfn_input_dispatch_ime_set_composition, jfn_input_dispatch_key_full,
    jfn_input_dispatch_keyboard_focus, jfn_input_dispatch_mouse_button,
    jfn_input_dispatch_mouse_move, jfn_input_dispatch_scroll,
};
use jfn_playback::shutdown::jfn_shutdown_initiate;

// =====================================================================
// Shared state. `set_cursor` is invoked from the CEF UI thread; the
// input thread reads `cursor_type` from WM_SETCURSOR. `input_hwnd_raw`
// and `thread_id` are written once during run_input_thread startup and
// read by the cross-thread set_cursor / stop / resize helpers.
// =====================================================================

struct State {
    input_hwnd_raw: usize,
    thread_id: u32,
    cursor_type: i32,
    geometry: InputGeometry,
    ime: ImeState,
}

static STATE: Mutex<State> = Mutex::new(State {
    input_hwnd_raw: 0,
    thread_id: 0,
    cursor_type: CursorShape::Pointer.as_raw(),
    geometry: InputGeometry::EMPTY,
    ime: ImeState::EMPTY,
});

#[derive(Clone, Debug, PartialEq, Eq)]
struct ImeState {
    input_language_id: u16,
    system_caret: bool,
    is_composing: bool,
    cursor_index: Option<u32>,
    composition_range: ImeTextRange,
    composition_bounds: Vec<JfnRect>,
}

impl ImeState {
    const EMPTY: Self = Self {
        input_language_id: 0x0409,
        system_caret: false,
        is_composing: false,
        cursor_index: None,
        composition_range: ImeTextRange { from: 0, to: 0 },
        composition_bounds: Vec::new(),
    };
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct InputGeometry {
    logical_w: i32,
    logical_h: i32,
    physical_w: i32,
    physical_h: i32,
}

impl InputGeometry {
    const EMPTY: Self = Self {
        logical_w: 0,
        logical_h: 0,
        physical_w: 0,
        physical_h: 0,
    };

    fn map_point(self, x: i32, y: i32) -> (i32, i32) {
        fn map_axis(value: i32, logical: i32, physical: i32) -> i32 {
            if logical <= 0 || physical <= 0 || logical == physical {
                return value;
            }
            (i64::from(value) * i64::from(logical) / i64::from(physical)) as i32
        }

        (
            map_axis(x, self.logical_w, self.physical_w),
            map_axis(y, self.logical_h, self.physical_h),
        )
    }

    fn map_rect_to_physical(self, rect: JfnRect) -> JfnRect {
        fn map_axis(value: i32, logical: i32, physical: i32) -> i32 {
            if logical <= 0 || physical <= 0 || logical == physical {
                return value;
            }
            (i64::from(value) * i64::from(physical) / i64::from(logical)) as i32
        }

        let left = map_axis(rect.x, self.logical_w, self.physical_w);
        let top = map_axis(rect.y, self.logical_h, self.physical_h);
        let right = map_axis(
            rect.x.saturating_add(rect.w),
            self.logical_w,
            self.physical_w,
        );
        let bottom = map_axis(
            rect.y.saturating_add(rect.h),
            self.logical_h,
            self.physical_h,
        );
        JfnRect {
            x: left,
            y: top,
            w: right.saturating_sub(left),
            h: bottom.saturating_sub(top),
        }
    }
}

fn map_client_point(x: i32, y: i32) -> (i32, i32) {
    STATE.lock().geometry.map_point(x, y)
}

// =====================================================================
// Win32 macro helpers — windows-rs doesn't ship the *_LPARAM / *_WPARAM
// macros from Windows headers, so reimplement the ones we need inline.
// =====================================================================

#[inline]
fn loword_u32(v: u32) -> u16 {
    (v & 0xFFFF) as u16
}

#[inline]
fn hiword_i16(v: u32) -> i16 {
    ((v >> 16) & 0xFFFF) as i16
}

#[inline]
fn get_x_lparam(lp: LPARAM) -> i32 {
    (lp.0 as i16) as i32
}
#[inline]
fn get_y_lparam(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) as i16) as i32
}

#[inline]
fn get_xbutton_wparam(wp: WPARAM) -> u16 {
    hiword_i16(wp.0 as u32) as u16
}

#[inline]
fn get_appcommand_lparam(lp: LPARAM) -> u16 {
    (hiword_i16(lp.0 as u32) as u16) & 0x7FFF
}

// =====================================================================
// Modifier helpers.
// =====================================================================

#[inline]
fn is_key_down(vk: u16) -> bool {
    let s = unsafe { GetKeyState(vk as i32) };
    (s as u16 & 0x8000) != 0
}

fn mouse_modifiers(wp: WPARAM) -> u32 {
    let mut m = 0u32;
    let w = wp.0 as u32;
    if w & MK_CONTROL.0 != 0 {
        m |= EVENTFLAG_CONTROL_DOWN;
    }
    if w & MK_SHIFT.0 != 0 {
        m |= EVENTFLAG_SHIFT_DOWN;
    }
    if is_key_down(VK_MENU.0) {
        m |= EVENTFLAG_ALT_DOWN;
    }
    if w & MK_LBUTTON.0 != 0 {
        m |= EVENTFLAG_LEFT_MOUSE_BUTTON;
    }
    if w & MK_RBUTTON.0 != 0 {
        m |= EVENTFLAG_RIGHT_MOUSE_BUTTON;
    }
    if w & MK_MBUTTON.0 != 0 {
        m |= EVENTFLAG_MIDDLE_MOUSE_BUTTON;
    }
    m
}

fn keyboard_modifiers(wp: WPARAM, lp: LPARAM) -> u32 {
    let mut m = 0u32;
    if is_key_down(VK_SHIFT.0) {
        m |= EVENTFLAG_SHIFT_DOWN;
    }
    if is_key_down(VK_CONTROL.0) {
        m |= EVENTFLAG_CONTROL_DOWN;
    }
    if is_key_down(VK_MENU.0) {
        m |= EVENTFLAG_ALT_DOWN;
    }
    if (unsafe { GetKeyState(VK_NUMLOCK.0 as i32) } & 1) != 0 {
        m |= EVENTFLAG_NUM_LOCK_ON;
    }
    if (unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1) != 0 {
        m |= EVENTFLAG_CAPS_LOCK_ON;
    }

    let extended = ((lp.0 >> 16) as u32 & KF_EXTENDED) != 0;
    let vk = wp.0 as u16;
    match vk {
        v if v == VK_RETURN.0 && extended => {
            m |= EVENTFLAG_IS_KEY_PAD;
        }
        v if !extended
            && (v == VK_INSERT.0
                || v == VK_DELETE.0
                || v == VK_HOME.0
                || v == VK_END.0
                || v == VK_PRIOR.0
                || v == VK_NEXT.0
                || v == VK_UP.0
                || v == VK_DOWN.0
                || v == VK_LEFT.0
                || v == VK_RIGHT.0) =>
        {
            m |= EVENTFLAG_IS_KEY_PAD;
        }
        v if v == VK_NUMLOCK.0
            || v == VK_NUMPAD0.0
            || v == VK_NUMPAD1.0
            || v == VK_NUMPAD2.0
            || v == VK_NUMPAD3.0
            || v == VK_NUMPAD4.0
            || v == VK_NUMPAD5.0
            || v == VK_NUMPAD6.0
            || v == VK_NUMPAD7.0
            || v == VK_NUMPAD8.0
            || v == VK_NUMPAD9.0
            || v == VK_DIVIDE.0
            || v == VK_MULTIPLY.0
            || v == VK_SUBTRACT.0
            || v == VK_ADD.0
            || v == VK_DECIMAL.0
            || v == VK_CLEAR.0 =>
        {
            m |= EVENTFLAG_IS_KEY_PAD;
        }
        v if v == VK_SHIFT.0 => {
            if is_key_down(VK_LSHIFT.0) {
                m |= EVENTFLAG_IS_LEFT;
            } else if is_key_down(VK_RSHIFT.0) {
                m |= EVENTFLAG_IS_RIGHT;
            }
        }
        v if v == VK_CONTROL.0 => {
            if is_key_down(VK_LCONTROL.0) {
                m |= EVENTFLAG_IS_LEFT;
            } else if is_key_down(VK_RCONTROL.0) {
                m |= EVENTFLAG_IS_RIGHT;
            }
        }
        v if v == VK_MENU.0 => {
            if is_key_down(VK_LMENU.0) {
                m |= EVENTFLAG_IS_LEFT;
            } else if is_key_down(VK_RMENU.0) {
                m |= EVENTFLAG_IS_RIGHT;
            }
        }
        v if v == VK_LWIN.0 => m |= EVENTFLAG_IS_LEFT,
        v if v == VK_RWIN.0 => m |= EVENTFLAG_IS_RIGHT,
        _ => {}
    }
    m
}

// =====================================================================
// Cursor mapping.
// =====================================================================

fn cef_cursor_to_win(shape: CursorShape) -> PCWSTR {
    use CursorShape::*;
    match shape {
        Cross => IDC_CROSS,
        Hand | Grab | Grabbing => IDC_HAND,
        IBeam => IDC_IBEAM,
        Wait => IDC_WAIT,
        Help => IDC_HELP,
        EastResize | WestResize | EastWestResize | ColumnResize => IDC_SIZEWE,
        NorthResize | SouthResize | NorthSouthResize | RowResize => IDC_SIZENS,
        NorthEastResize | SouthWestResize | NorthEastSouthWestResize => IDC_SIZENESW,
        NorthWestResize | SouthEastResize | NorthWestSouthEastResize => IDC_SIZENWSE,
        Move | MiddlePanning | MiddlePanningVertical | MiddlePanningHorizontal => IDC_SIZEALL,
        Progress => IDC_APPSTARTING,
        NoDrop | NotAllowed => IDC_NO,
        _ => IDC_ARROW,
    }
}

// =====================================================================
// Mouse button helpers.
// =====================================================================

fn msg_to_button_code(msg: u32) -> u32 {
    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK => BTN_LEFT,
        WM_RBUTTONDOWN | WM_RBUTTONUP | WM_RBUTTONDBLCLK => BTN_RIGHT,
        WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MBUTTONDBLCLK => BTN_MIDDLE,
        _ => BTN_LEFT,
    }
}

#[inline]
fn is_button_down(msg: u32) -> bool {
    matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN)
}

// =====================================================================
// IMM32 composition handling for windowless CEF.
// =====================================================================

fn parse_keyboard_layout_language_id(layout: &[u16; 9]) -> Option<u16> {
    let suffix = String::from_utf16(&layout[4..8]).ok()?;
    u16::from_str_radix(&suffix, 16).ok()
}

fn current_input_language_id() -> u16 {
    let mut layout = [0_u16; 9];
    if unsafe { GetKeyboardLayoutNameW(&mut layout) }.is_ok() {
        parse_keyboard_layout_language_id(&layout).unwrap_or(0x0409)
    } else {
        0x0409
    }
}

fn primary_language(language_id: u16) -> u32 {
    u32::from(language_id & 0x03FF)
}

fn create_ime_window(hwnd: HWND) {
    let language_id = current_input_language_id();
    let should_create_caret = matches!(primary_language(language_id), LANG_CHINESE | LANG_JAPANESE);
    {
        let mut state = STATE.lock();
        state.ime.input_language_id = language_id;
        if state.ime.system_caret || !should_create_caret {
            return;
        }
    }
    if unsafe { CreateCaret(hwnd, None, 1, 1) }.is_ok() {
        STATE.lock().ime.system_caret = true;
    } else {
        tracing::warn!("failed to create temporary IME caret");
    }
}

fn destroy_ime_window() {
    let should_destroy = {
        let mut state = STATE.lock();
        std::mem::take(&mut state.ime.system_caret)
    };
    if should_destroy && unsafe { DestroyCaret() }.is_err() {
        tracing::warn!("failed to destroy temporary IME caret");
    }
}

fn reset_ime_composition() {
    let mut state = STATE.lock();
    state.ime.is_composing = false;
    state.ime.cursor_index = None;
}

fn move_ime_window(hwnd: HWND) {
    if unsafe { GetFocus() } != hwnd {
        return;
    }
    let (language_id, system_caret, mut rect) = {
        let state = STATE.lock();
        let range = state.ime.composition_range;
        let mut location = state.ime.cursor_index.unwrap_or(range.from);
        if location >= range.from {
            location -= range.from;
        }
        let Some(rect) = state.ime.composition_bounds.get(location as usize) else {
            return;
        };
        (state.ime.input_language_id, state.ime.system_caret, *rect)
    };

    let imc = unsafe { ImmGetContext(hwnd) };
    if imc.0.is_null() {
        tracing::warn!("IMM32 returned no input context while positioning candidate window");
        return;
    }

    let language = primary_language(language_id);
    if language == LANG_CHINESE {
        let candidate = CANDIDATEFORM {
            dwIndex: 0,
            dwStyle: CFS_CANDIDATEPOS,
            ptCurrentPos: POINT {
                x: rect.x,
                y: rect.y,
            },
            rcArea: RECT::default(),
        };
        if !unsafe { ImmSetCandidateWindow(imc, &candidate) }.as_bool() {
            tracing::warn!("ImmSetCandidateWindow(CFS_CANDIDATEPOS) failed");
        }
    }

    if system_caret {
        let caret_y = if language == LANG_JAPANESE {
            rect.y.saturating_add(rect.h)
        } else {
            rect.y
        };
        if unsafe { SetCaretPos(rect.x, caret_y) }.is_err() {
            tracing::warn!("failed to move temporary IME caret");
        }
    }

    if language == LANG_KOREAN {
        rect.y = rect.y.saturating_add(1);
    }
    let exclude = CANDIDATEFORM {
        dwIndex: 0,
        dwStyle: CFS_EXCLUDE,
        ptCurrentPos: POINT {
            x: rect.x,
            y: rect.y,
        },
        rcArea: RECT {
            left: rect.x,
            top: rect.y,
            right: rect.x.saturating_add(rect.w),
            bottom: rect.y.saturating_add(rect.h),
        },
    };
    if !unsafe { ImmSetCandidateWindow(imc, &exclude) }.as_bool() {
        tracing::warn!("ImmSetCandidateWindow(CFS_EXCLUDE) failed");
    }
    let _ = unsafe { ImmReleaseContext(hwnd, imc) };
}

fn read_ime_blob(
    imc: HIMC,
    flags: u32,
    kind: windows::Win32::UI::Input::Ime::IME_COMPOSITION_STRING,
) -> Result<Option<Vec<u8>>, String> {
    if flags & kind.0 == 0 {
        return Ok(None);
    }
    let size = unsafe { ImmGetCompositionStringW(imc, kind, None, 0) };
    if size < 0 {
        return Err(format!(
            "ImmGetCompositionStringW({:#x}) size failed: {size}",
            kind.0
        ));
    }
    if size == 0 {
        return Ok(None);
    }
    let mut data = vec![0_u8; size as usize];
    let read = unsafe {
        ImmGetCompositionStringW(imc, kind, Some(data.as_mut_ptr().cast()), data.len() as u32)
    };
    if read != size {
        return Err(format!(
            "ImmGetCompositionStringW({:#x}) returned {read} bytes, expected {size}",
            kind.0
        ));
    }
    Ok(Some(data))
}

fn read_ime_string(
    imc: HIMC,
    flags: u32,
    kind: windows::Win32::UI::Input::Ime::IME_COMPOSITION_STRING,
) -> Result<Option<String>, String> {
    let Some(data) = read_ime_blob(imc, flags, kind)? else {
        return Ok(None);
    };
    let mut chunks = data.chunks_exact(2);
    let utf16: Vec<u16> = chunks
        .by_ref()
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    if !chunks.remainder().is_empty() {
        return Err(format!(
            "ImmGetCompositionStringW({:#x}) returned an odd byte count",
            kind.0
        ));
    }
    String::from_utf16(&utf16)
        .map(Some)
        .map_err(|error| format!("invalid UTF-16 from IMM32: {error}"))
}

fn read_ime_clauses(imc: HIMC, flags: u32) -> Result<Option<Vec<u32>>, String> {
    let Some(data) = read_ime_blob(imc, flags, GCS_COMPCLAUSE)? else {
        return Ok(None);
    };
    let mut chunks = data.chunks_exact(4);
    let clauses: Vec<u32> = chunks
        .by_ref()
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    if !chunks.remainder().is_empty() {
        return Err("IMM32 composition clauses were not aligned to u32".to_owned());
    }
    Ok(Some(clauses))
}

fn composition_target_range(attributes: Option<&[u8]>, length: u32) -> Result<(u32, u32), String> {
    let Some(attributes) = attributes else {
        return Ok((length, length));
    };
    if attributes.len() != length as usize {
        return Err(format!(
            "IMM32 returned {} attributes for {length} UTF-16 code units",
            attributes.len()
        ));
    }
    let is_target = |value: u8| {
        u32::from(value) == ATTR_TARGET_CONVERTED || u32::from(value) == ATTR_TARGET_NOTCONVERTED
    };
    let start = attributes
        .iter()
        .position(|value| is_target(*value))
        .unwrap_or(attributes.len());
    let end = attributes[start..]
        .iter()
        .position(|value| !is_target(*value))
        .map_or(attributes.len(), |offset| start + offset);
    Ok((start as u32, end as u32))
}

fn build_ime_underlines(
    length: u32,
    attributes: Option<&[u8]>,
    clauses: Option<&[u32]>,
) -> Result<Vec<ImeUnderline>, String> {
    let (target_start, target_end) = composition_target_range(attributes, length)?;
    let mut underlines = Vec::new();
    if let Some(clauses) = clauses {
        for pair in clauses.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            if from > to || to > length {
                return Err(format!(
                    "invalid IMM32 composition clause {from}..{to} for length {length}"
                ));
            }
            underlines.push(ImeUnderline {
                from,
                to,
                thick: from >= target_start && to <= target_end,
            });
        }
    }
    if underlines.is_empty() {
        if target_start > 0 {
            underlines.push(ImeUnderline {
                from: 0,
                to: target_start,
                thick: false,
            });
        }
        if target_end > target_start {
            underlines.push(ImeUnderline {
                from: target_start,
                to: target_end,
                thick: true,
            });
        }
        if target_end < length {
            underlines.push(ImeUnderline {
                from: target_end,
                to: length,
                thick: false,
            });
        }
    }
    Ok(underlines)
}

struct ImeComposition {
    text: String,
    underlines: Vec<ImeUnderline>,
    cursor: u32,
    utf16_len: u32,
}

fn read_ime_composition(imc: HIMC, flags: u32) -> Result<Option<ImeComposition>, String> {
    let Some(text) = read_ime_string(imc, flags, GCS_COMPSTR)? else {
        return Ok(None);
    };
    let utf16_len = text.encode_utf16().count() as u32;
    let attributes = read_ime_blob(imc, flags, GCS_COMPATTR)?;
    let clauses = read_ime_clauses(imc, flags)?;
    let underlines = build_ime_underlines(utf16_len, attributes.as_deref(), clauses.as_deref())?;
    let cursor = if flags & CS_NOMOVECARET == 0 && flags & GCS_CURSORPOS.0 != 0 {
        let cursor = unsafe { ImmGetCompositionStringW(imc, GCS_CURSORPOS, None, 0) };
        if cursor < 0 {
            return Err(format!(
                "ImmGetCompositionStringW(GCS_CURSORPOS) failed: {cursor}"
            ));
        }
        cursor as u32
    } else {
        0
    };
    if cursor > utf16_len {
        return Err(format!(
            "IMM32 cursor {cursor} exceeds composition length {utf16_len}"
        ));
    }
    Ok(Some(ImeComposition {
        text,
        underlines,
        cursor,
        utf16_len,
    }))
}

fn cancel_ime_composition() {
    jfn_input_dispatch_ime_cancel_composition();
    reset_ime_composition();
    destroy_ime_window();
}

fn handle_ime_composition(hwnd: HWND, flags: u32) {
    let imc = unsafe { ImmGetContext(hwnd) };
    if imc.0.is_null() {
        tracing::error!("WM_IME_COMPOSITION arrived without an IMM32 context");
        cancel_ime_composition();
        return;
    }
    let payload = (|| {
        let result = read_ime_string(imc, flags, GCS_RESULTSTR)?;
        let composition = read_ime_composition(imc, flags)?;
        Ok::<_, String>((result, composition))
    })();
    let _ = unsafe { ImmReleaseContext(hwnd, imc) };

    let (result, composition) = match payload {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, "failed to decode IMM32 composition");
            cancel_ime_composition();
            return;
        }
    };
    if let Some(result) = result {
        jfn_input_dispatch_ime_commit_text(&result);
        reset_ime_composition();
    }
    if let Some(composition) = composition {
        jfn_input_dispatch_ime_set_composition(
            &composition.text,
            &composition.underlines,
            ImeTextRange {
                from: composition.cursor,
                to: composition.cursor.saturating_add(composition.utf16_len),
            },
        );
        {
            let mut state = STATE.lock();
            state.ime.is_composing = true;
            state.ime.cursor_index = composition.cursor.checked_sub(1);
        }
        move_ime_window(hwnd);
    } else {
        cancel_ime_composition();
    }
}

// =====================================================================
// WndProc.
// =====================================================================

unsafe extern "system" fn input_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_IME_SETCONTEXT => {
            let filtered = LPARAM(lp.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize));
            let _ = unsafe { DefWindowProcW(hwnd, msg, wp, filtered) };
            create_ime_window(hwnd);
            move_ime_window(hwnd);
            return LRESULT(0);
        }

        WM_IME_STARTCOMPOSITION => {
            create_ime_window(hwnd);
            move_ime_window(hwnd);
            reset_ime_composition();
            return LRESULT(0);
        }

        WM_IME_COMPOSITION => {
            handle_ime_composition(hwnd, lp.0 as u32);
            return LRESULT(0);
        }

        WM_IME_ENDCOMPOSITION => {
            cancel_ime_composition();
            return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
        }

        WM_SETCURSOR if u32::from(loword_u32(lp.0 as u32)) == HTCLIENT => {
            let shape =
                CursorShape::from_cef(STATE.lock().cursor_type).unwrap_or(CursorShape::Pointer);
            if shape == CursorShape::None {
                unsafe { SetCursor(None) };
            } else {
                let cur = unsafe { LoadCursorW(None, cef_cursor_to_win(shape)).ok() };
                unsafe { SetCursor(cur) };
            }
            return LRESULT(1);
        }

        WM_MOUSEMOVE => {
            let (x, y) = map_client_point(get_x_lparam(lp), get_y_lparam(lp));
            jfn_input_dispatch_mouse_move(x, y, mouse_modifiers(wp), 0);
            return LRESULT(0);
        }

        WM_MOUSELEAVE => {
            jfn_input_dispatch_mouse_move(-1, -1, mouse_modifiers(wp), 1);
            return LRESULT(0);
        }

        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP
        | WM_MBUTTONUP => {
            let down = is_button_down(msg);
            if down {
                let _ = unsafe { SetFocus(Some(hwnd)) };
            }
            let (x, y) = map_client_point(get_x_lparam(lp), get_y_lparam(lp));
            jfn_input_dispatch_mouse_button(
                msg_to_button_code(msg),
                if down { 1 } else { 0 },
                x,
                y,
                mouse_modifiers(wp),
            );
            return LRESULT(0);
        }

        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let btn = get_xbutton_wparam(wp);
            if msg == WM_XBUTTONDOWN {
                let fwd = if btn == XBUTTON2 { 1 } else { 0 };
                jfn_input_dispatch_history_nav(fwd);
            }
            return LRESULT(1); // TRUE per MSDN
        }

        WM_APPCOMMAND => {
            let cmd = get_appcommand_lparam(lp) as u32;
            if cmd == APPCOMMAND_BROWSER_BACKWARD.0 {
                jfn_input_dispatch_history_nav(0);
                return LRESULT(1);
            }
            if cmd == APPCOMMAND_BROWSER_FORWARD.0 {
                jfn_input_dispatch_history_nav(1);
                return LRESULT(1);
            }
            // bubble unhandled commands to parent via DefWindowProc.
        }

        WM_MOUSEWHEEL => {
            let mut pt = POINT {
                x: get_x_lparam(lp),
                y: get_y_lparam(lp),
            };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut pt);
            }
            let (x, y) = map_client_point(pt.x, pt.y);
            let delta = hiword_i16(wp.0 as u32) as i32;
            jfn_input_dispatch_scroll(x, y, 0, delta, mouse_modifiers(wp));
            return LRESULT(0);
        }

        WM_MOUSEHWHEEL => {
            let mut pt = POINT {
                x: get_x_lparam(lp),
                y: get_y_lparam(lp),
            };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut pt);
            }
            let (x, y) = map_client_point(pt.x, pt.y);
            let delta = hiword_i16(wp.0 as u32) as i32;
            jfn_input_dispatch_scroll(x, y, delta, 0, mouse_modifiers(wp));
            return LRESULT(0);
        }

        WM_APP_FOCUS_INPUT => {
            let _ = unsafe { SetFocus(Some(hwnd)) };
            return LRESULT(0);
        }

        WM_APP_IME_POSITION => {
            move_ime_window(hwnd);
            return LRESULT(0);
        }

        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let vk = wp.0 as u16;
            if vk == VK_F4.0 && msg == WM_SYSKEYDOWN && is_key_down(VK_MENU.0) {
                jfn_shutdown_initiate();
                return LRESULT(0);
            }
            // Browser nav keystrokes (some IR drivers).
            if vk == VK_BROWSER_BACK.0 || vk == VK_BROWSER_FORWARD.0 {
                if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                    let fwd = if vk == VK_BROWSER_FORWARD.0 { 1 } else { 0 };
                    jfn_input_dispatch_history_nav(fwd);
                }
                return LRESULT(0);
            }
            let pressed = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let is_sys = msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP;
            jfn_input_dispatch_key_full(
                if pressed { 1 } else { 0 },
                vk as i32,
                lp.0 as i32,
                keyboard_modifiers(wp, lp),
                0,
                0,
                if is_sys { 1 } else { 0 },
            );
            return LRESULT(0);
        }

        WM_CHAR | WM_SYSCHAR => {
            jfn_input_dispatch_char_sys(
                wp.0 as u32,
                keyboard_modifiers(wp, lp),
                lp.0 as u32,
                if msg == WM_SYSCHAR { 1 } else { 0 },
            );
            return LRESULT(0);
        }

        WM_SETFOCUS => {
            jfn_input_dispatch_keyboard_focus(1);
            return LRESULT(0);
        }
        WM_KILLFOCUS => {
            cancel_ime_composition();
            jfn_input_dispatch_keyboard_focus(0);
            return LRESULT(0);
        }

        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

// =====================================================================
// Thread entry — registers the class, creates the child window, runs
// the message loop, then cleans up. Called from std::thread::spawn in
// platform.rs::win_init.
// =====================================================================

const CLASS_NAME: PCWSTR = w!("JellyfinCefInput");

pub fn jfn_input_windows_run_input_thread(mpv_hwnd: *mut std::ffi::c_void) {
    let mpv = HWND(mpv_hwnd);
    let tid = unsafe { GetCurrentThreadId() };

    {
        let mut s = STATE.lock();
        s.thread_id = tid;
    }

    let hinst = unsafe { GetModuleHandleW(None).unwrap_or_default() };

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: Default::default(),
        lpfnWndProc: Some(input_wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst.into(),
        hIcon: HICON::default(),
        // No class cursor — WM_SETCURSOR drives it.
        hCursor: HCURSOR::default(),
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: CLASS_NAME,
        hIconSm: HICON::default(),
    };
    unsafe { RegisterClassExW(&wc) };

    let mut rc = RECT::default();
    let _ = unsafe { GetClientRect(mpv, &mut rc) };
    let physical_w = rc.right - rc.left;
    let physical_h = rc.bottom - rc.top;
    let scale = crate::platform::win_get_scale().max(1.0);
    let fallback_geometry = InputGeometry {
        logical_w: (physical_w as f32 / scale).round() as i32,
        logical_h: (physical_h as f32 / scale).round() as i32,
        physical_w,
        physical_h,
    };
    let geometry = {
        let mut state = STATE.lock();
        let geometry = initial_geometry(state.geometry, fallback_geometry);
        state.geometry = geometry;
        geometry
    };

    let input_hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!(""),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
            0,
            0,
            geometry.physical_w,
            geometry.physical_h,
            Some(mpv),
            Some(HMENU(std::ptr::null_mut())),
            Some(hinst.into()),
            None,
        )
    };

    let input_hwnd = match input_hwnd {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("CreateWindowExW(JellyfinCefInput) failed: {e:?}");
            STATE.lock().thread_id = 0;
            return;
        }
    };
    STATE.lock().input_hwnd_raw = input_hwnd.0 as usize;

    // Share input queue with mpv so SetFocus across windows works.
    let mpv_tid = unsafe { GetWindowThreadProcessId(mpv, None) };
    let _ = unsafe { AttachThreadInput(tid, mpv_tid, true) };
    let _ = unsafe { SetFocus(Some(input_hwnd)) };

    // Standard GetMessage/Dispatch loop.
    let mut m = MSG::default();
    while unsafe { GetMessageW(&mut m, None, 0, 0).0 } > 0 {
        unsafe {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }

    destroy_ime_window();
    if STATE.lock().input_hwnd_raw != 0 {
        let _ = unsafe { DestroyWindow(input_hwnd) };
        STATE.lock().input_hwnd_raw = 0;
    }
    let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(hinst.into())) };
    STATE.lock().thread_id = 0;
}

fn initial_geometry(current: InputGeometry, fallback: InputGeometry) -> InputGeometry {
    // A WindowExtent notification can arrive after the input thread starts but
    // before this child window is created. Keep that exact CEF geometry when
    // it matches the parent client bounds; otherwise Win32 is the only source
    // available for the initial physical size.
    if current.logical_w > 0
        && current.logical_h > 0
        && current.physical_w == fallback.physical_w
        && current.physical_h == fallback.physical_h
    {
        current
    } else {
        fallback
    }
}

pub fn jfn_input_windows_stop_input_thread() {
    let tid = STATE.lock().thread_id;
    if tid != 0 {
        let _ = unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}

pub fn jfn_input_windows_resize_to_parent(
    logical_w: c_int,
    logical_h: c_int,
    physical_w: c_int,
    physical_h: c_int,
) {
    let hwnd_raw = {
        let mut state = STATE.lock();
        state.geometry = InputGeometry {
            logical_w,
            logical_h,
            physical_w,
            physical_h,
        };
        state.input_hwnd_raw
    };
    if hwnd_raw == 0 {
        return;
    }
    let hwnd = HWND(hwnd_raw as *mut _);
    let flags: SET_WINDOW_POS_FLAGS =
        SET_WINDOW_POS_FLAGS(SWP_NOZORDER.0 | SWP_NOMOVE.0 | SWP_NOACTIVATE.0);
    let _ = unsafe { SetWindowPos(hwnd, None, 0, 0, physical_w, physical_h, flags) };
}

pub fn jfn_input_windows_focus() {
    let hwnd_raw = STATE.lock().input_hwnd_raw;
    if hwnd_raw == 0 {
        return;
    }
    let hwnd = HWND(hwnd_raw as *mut _);
    let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_FOCUS_INPUT, WPARAM(0), LPARAM(0)) };
}

/// Platform::set_cursor — invoked from the CEF UI thread. Stores the
/// pending cursor type and posts a synthetic WM_SETCURSOR so the input
/// thread applies it via SetCursor (which is thread-affine).
pub fn jfn_input_windows_set_cursor(t: c_int) {
    let hwnd_raw = {
        let mut s = STATE.lock();
        s.cursor_type = t;
        s.input_hwnd_raw
    };
    if hwnd_raw == 0 {
        return;
    }
    let hwnd = HWND(hwnd_raw as *mut _);
    // wparam = hwnd, lparam = MAKELPARAM(HTCLIENT=1, 0)
    let _ = unsafe { PostMessageW(Some(hwnd), WM_SETCURSOR, WPARAM(hwnd_raw), LPARAM(1)) };
}

pub fn jfn_input_windows_set_ime_composition_range(
    selected_range: ImeTextRange,
    character_bounds: &[JfnRect],
) {
    let hwnd_raw = {
        let mut state = STATE.lock();
        let geometry = state.geometry;
        state.ime.composition_range = selected_range;
        state.ime.composition_bounds = character_bounds
            .iter()
            .copied()
            .map(|rect| geometry.map_rect_to_physical(rect))
            .collect();
        state.input_hwnd_raw
    };
    if hwnd_raw == 0 {
        return;
    }
    let hwnd = HWND(hwnd_raw as *mut _);
    let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_IME_POSITION, WPARAM(0), LPARAM(0)) };
}

#[cfg(test)]
mod tests {
    use super::{
        InputGeometry, build_ime_underlines, composition_target_range, initial_geometry,
        parse_keyboard_layout_language_id,
    };
    use jfn_platform_abi::{ImeUnderline, JfnRect};

    #[test]
    fn maps_physical_client_coordinates_to_cef_logical_coordinates() {
        let geometry = InputGeometry {
            logical_w: 1048,
            logical_h: 551,
            physical_w: 1311,
            physical_h: 689,
        };

        assert_eq!(geometry.map_point(169, 603), (135, 482));
        assert_eq!(geometry.map_point(1310, 688), (1047, 550));
    }

    #[test]
    fn preserves_mpv_extent_geometry_during_input_thread_startup() {
        let synced = InputGeometry {
            logical_w: 874,
            logical_h: 491,
            physical_w: 1311,
            physical_h: 736,
        };
        let stale_scale_fallback = InputGeometry {
            logical_w: 1049,
            logical_h: 589,
            physical_w: 1311,
            physical_h: 736,
        };

        assert_eq!(initial_geometry(synced, stale_scale_fallback), synced);
    }

    #[test]
    fn startup_uses_parent_bounds_when_synced_extent_is_stale() {
        let stale = InputGeometry {
            logical_w: 1049,
            logical_h: 589,
            physical_w: 1311,
            physical_h: 736,
        };
        let parent_bounds = InputGeometry {
            logical_w: 1280,
            logical_h: 720,
            physical_w: 1600,
            physical_h: 900,
        };

        assert_eq!(initial_geometry(stale, parent_bounds), parent_bounds);
    }

    #[test]
    fn identity_and_unknown_geometry_keep_coordinates_unchanged() {
        let identity = InputGeometry {
            logical_w: 1920,
            logical_h: 1080,
            physical_w: 1920,
            physical_h: 1080,
        };

        assert_eq!(identity.map_point(640, 360), (640, 360));
        assert_eq!(InputGeometry::EMPTY.map_point(640, 360), (640, 360));
    }

    #[test]
    fn maps_cef_logical_ime_bounds_to_physical_client_coordinates() {
        let geometry = InputGeometry {
            logical_w: 1048,
            logical_h: 551,
            physical_w: 1311,
            physical_h: 689,
        };

        assert_eq!(
            geometry.map_rect_to_physical(JfnRect {
                x: 135,
                y: 482,
                w: 12,
                h: 20,
            }),
            JfnRect {
                x: 168,
                y: 602,
                w: 15,
                h: 25,
            }
        );
    }

    #[test]
    fn parses_hex_language_id_from_windows_keyboard_layout_name() {
        let layout = [
            b'0' as u16,
            b'0' as u16,
            b'0' as u16,
            b'0' as u16,
            b'0' as u16,
            b'8' as u16,
            b'0' as u16,
            b'4' as u16,
            0,
        ];
        assert_eq!(parse_keyboard_layout_language_id(&layout), Some(0x0804));
    }

    #[test]
    fn derives_target_range_and_default_underlines_from_ime_attributes() {
        let attributes = [0, 1, 1, 0];
        assert_eq!(composition_target_range(Some(&attributes), 4), Ok((1, 3)));
        assert_eq!(
            build_ime_underlines(4, Some(&attributes), None),
            Ok(vec![
                ImeUnderline {
                    from: 0,
                    to: 1,
                    thick: false,
                },
                ImeUnderline {
                    from: 1,
                    to: 3,
                    thick: true,
                },
                ImeUnderline {
                    from: 3,
                    to: 4,
                    thick: false,
                },
            ])
        );
    }

    #[test]
    fn uses_valid_ime_clauses_and_rejects_out_of_range_data() {
        assert_eq!(
            build_ime_underlines(4, None, Some(&[0, 2, 4])),
            Ok(vec![
                ImeUnderline {
                    from: 0,
                    to: 2,
                    thick: false,
                },
                ImeUnderline {
                    from: 2,
                    to: 4,
                    thick: false,
                },
            ])
        );
        assert!(build_ime_underlines(4, None, Some(&[0, 5])).is_err());
        assert!(composition_target_range(Some(&[1, 1]), 3).is_err());
    }
}
