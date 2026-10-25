//! Post-init mpv handle accessors used by sibling crates.
//!
//! All entry points borrow the global handle published by
//! [`crate::boot::jfn_mpv_handle_init`]. They no-op silently if the
//! handle has not yet been initialized or has already been terminated.
//!
//! Property writes and commands go through libmpv's async API
//! (`reply_userdata == 0`, fire-and-forget). Property reads are
//! synchronous and must only be issued from non-event contexts; observed
//! properties should be read from the `jfn_playback_*` atomics instead.
//!
//! Stateful helpers — `LoadFile` / `ApplyPendingTrackSelectionAndPlay`
//! / `SetAspectMode` — live here too. The pending-track state is
//! single-threaded by usage but guarded by a Mutex so callers from any
//! thread stay safe.
//!
//! # Safety
//!
//! Every `pub unsafe fn` in this module accepts raw C-string / raw struct
//! pointers preserved from the original FFI surface. Callers must ensure
//! all `*const c_char` arguments point to NUL-terminated UTF-8 (or are
//! null where the function documents tolerance), and that struct
//! pointers reference live values for the duration of the call.

#![allow(clippy::missing_safety_doc)]

use parking_lot::Mutex;
use std::ffi::{CStr, CString, c_char};
use std::os::raw::c_void;

use crate::{
    command::{LoadFileCommand, NamedArgsCommand, overlay_arguments},
    sys,
};

// =============================================================================
// Internal helpers
// =============================================================================

fn raw() -> *mut sys::mpv_handle {
    crate::boot::current_raw_handle().unwrap_or(std::ptr::null_mut())
}

fn checked_raw() -> crate::Result<*mut sys::mpv_handle> {
    let handle = raw();
    if handle.is_null() {
        Err(crate::Error::new(sys::mpv_error::MPV_ERROR_UNINITIALIZED.0))
    } else {
        Ok(handle)
    }
}

unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a CStr> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) })
    }
}

// =============================================================================
// Generic property R/W + command
// =============================================================================

/// Async (`reply_userdata == 0`) flag write. No-op if the handle is
/// missing or `name` is NULL.
pub unsafe fn jfn_mpv_set_property_flag_async(name: *const c_char, value: bool) {
    let h = raw();
    if h.is_null() {
        return;
    }
    let Some(n) = (unsafe { cstr(name) }) else {
        return;
    };
    let mut flag: i32 = if value { 1 } else { 0 };
    unsafe {
        sys::mpv_set_property_async(
            h,
            0,
            n.as_ptr(),
            sys::mpv_format::MPV_FORMAT_FLAG,
            &mut flag as *mut _ as *mut c_void,
        );
    }
}

pub unsafe fn jfn_mpv_set_property_double_async(name: *const c_char, value: f64) {
    let h = raw();
    if h.is_null() {
        return;
    }
    let Some(n) = (unsafe { cstr(name) }) else {
        return;
    };
    let mut v = value;
    unsafe {
        sys::mpv_set_property_async(
            h,
            0,
            n.as_ptr(),
            sys::mpv_format::MPV_FORMAT_DOUBLE,
            &mut v as *mut _ as *mut c_void,
        );
    }
}

pub unsafe fn jfn_mpv_set_property_int_async(name: *const c_char, value: i64) {
    let h = raw();
    if h.is_null() {
        return;
    }
    let Some(n) = (unsafe { cstr(name) }) else {
        return;
    };
    let mut v = value;
    unsafe {
        sys::mpv_set_property_async(
            h,
            0,
            n.as_ptr(),
            sys::mpv_format::MPV_FORMAT_INT64,
            &mut v as *mut _ as *mut c_void,
        );
    }
}

pub unsafe fn jfn_mpv_set_property_string_async(name: *const c_char, value: *const c_char) {
    let h = raw();
    if h.is_null() {
        return;
    }
    let Some(n) = (unsafe { cstr(name) }) else {
        return;
    };
    let Some(v) = (unsafe { cstr(value) }) else {
        return;
    };
    let mut ptr = v.as_ptr();
    unsafe {
        sys::mpv_set_property_async(
            h,
            0,
            n.as_ptr(),
            sys::mpv_format::MPV_FORMAT_STRING,
            &mut ptr as *mut _ as *mut c_void,
        );
    }
}

/// Sync int property read. Writes the value into `*out` and returns
/// libmpv's error code (0 on success, negative on failure). NULL `out`
/// or missing handle returns `MPV_ERROR_INVALID_PARAMETER` (-4).
pub unsafe fn jfn_mpv_get_property_int(name: *const c_char, out: *mut i64) -> i32 {
    let h = raw();
    if h.is_null() || out.is_null() {
        return -4;
    }
    let Some(n) = (unsafe { cstr(name) }) else {
        return -4;
    };
    unsafe {
        sys::mpv_get_property(
            h,
            n.as_ptr(),
            sys::mpv_format::MPV_FORMAT_INT64,
            out as *mut c_void,
        )
    }
}

/// Sync double property read. Writes the value into `*out` and returns
/// libmpv's error code (0 on success, negative on failure).
pub unsafe fn jfn_mpv_get_property_double(name: *const c_char, out: *mut f64) -> i32 {
    let h = raw();
    if h.is_null() || out.is_null() {
        return -4;
    }
    let Some(n) = (unsafe { cstr(name) }) else {
        return -4;
    };
    unsafe {
        sys::mpv_get_property(
            h,
            n.as_ptr(),
            sys::mpv_format::MPV_FORMAT_DOUBLE,
            out as *mut c_void,
        )
    }
}

/// Sync string property read. Returns a malloc'd UTF-8 C string the
/// caller must free with [`jfn_mpv_free_string`], or NULL on failure.
pub unsafe fn jfn_mpv_get_property_string(name: *const c_char) -> *mut c_char {
    let h = raw();
    if h.is_null() {
        return std::ptr::null_mut();
    }
    let Some(n) = (unsafe { cstr(name) }) else {
        return std::ptr::null_mut();
    };
    let p = unsafe { sys::mpv_get_property_string(h, n.as_ptr()) };
    if p.is_null() {
        return std::ptr::null_mut();
    }
    // libmpv owns p (mpv_free required). Copy into a Rust-allocated
    // CString so the caller's free pairs with `jfn_mpv_free_string`.
    let out = unsafe { CStr::from_ptr(p) }.to_owned();
    unsafe { sys::mpv_free(p as *mut c_void) };
    out.into_raw()
}

/// Sync node property read. The returned tree owns all copied values.
/// Must not be called from the mpv event thread.
pub fn jfn_mpv_get_property_node(name: &CStr) -> crate::Result<crate::Node> {
    let handle = checked_raw()?;
    let mut raw_node: sys::mpv_node = unsafe { std::mem::zeroed() };
    crate::error::check(unsafe {
        sys::mpv_get_property(
            handle,
            name.as_ptr(),
            sys::mpv_format::MPV_FORMAT_NODE,
            &mut raw_node as *mut _ as *mut c_void,
        )
    })?;
    let node = unsafe { crate::Node::from_raw(&raw_node) };
    unsafe { sys::mpv_free_node_contents(&mut raw_node) };
    Ok(node)
}

pub unsafe fn jfn_mpv_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Async command. `args` is a `const char* const*` table of length `n`
/// (no NULL terminator required — the wrapper appends one). No-op on
/// missing handle, empty argv, or NULL entries.
pub unsafe fn jfn_mpv_command_async(args: *const *const c_char, n: usize) {
    let h = raw();
    if h.is_null() || args.is_null() || n == 0 {
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(args, n) };
    if slice.iter().any(|p| p.is_null()) {
        return;
    }
    let mut argv: Vec<*const c_char> = slice.to_vec();
    argv.push(std::ptr::null());
    unsafe { sys::mpv_command_async(h, 0, argv.as_ptr() as *mut _) };
}

// =============================================================================
// Event drain (wait_event / wakeup).
// =============================================================================

#[derive(Clone, Debug, PartialEq)]
pub enum WaitEvent {
    None,
    LogMessage(crate::LogMessage),
    Event(crate::Event),
}

/// Pumps libmpv's event queue. Returns the raw `mpv_event*` libmpv owns;
/// valid only until the next call on the same handle. NULL if the handle
/// is missing.
pub fn jfn_mpv_wait_event(timeout: f64) -> *mut sys::mpv_event {
    let h = raw();
    if h.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { sys::mpv_wait_event(h, timeout) }
}

/// A missing handle and `MPV_EVENT_NONE` both collapse to [`WaitEvent::None`].
pub fn wait_event_owned(timeout: f64) -> WaitEvent {
    let ev = jfn_mpv_wait_event(timeout);
    if ev.is_null() {
        return WaitEvent::None;
    }
    match unsafe { crate::Event::from_raw(ev) } {
        crate::Event::None => WaitEvent::None,
        crate::Event::LogMessage(m) => WaitEvent::LogMessage(m),
        event => WaitEvent::Event(event),
    }
}

pub fn jfn_mpv_wakeup() {
    let h = raw();
    if !h.is_null() {
        unsafe { sys::mpv_wakeup(h) };
    }
}

/// Install a C-style wakeup callback against the singleton mpv handle. The
/// callback fires from a foreign thread whenever libmpv queues a new
/// event; per the libmpv docs it must return promptly and call no
/// blocking API.
///
/// # Safety
/// `cb` must remain valid for as long as the mpv handle is in use.
pub unsafe fn jfn_mpv_set_wakeup_callback(
    cb: unsafe extern "C" fn(*mut std::ffi::c_void),
    data: *mut std::ffi::c_void,
) {
    let h = raw();
    if !h.is_null() {
        unsafe { sys::mpv_set_wakeup_callback(h, Some(cb), data) };
    }
}

/// Clear any previously-installed wakeup callback. After this call libmpv
/// will not fire a foreign-thread notification on new events.
pub fn jfn_mpv_clear_wakeup_callback() {
    let h = raw();
    if !h.is_null() {
        unsafe { sys::mpv_set_wakeup_callback(h, None, std::ptr::null_mut()) };
    }
}

// =============================================================================
// Player API — convenience wrappers over property writes / commands.
// =============================================================================

unsafe fn set_flag(name: &CStr, v: bool) {
    unsafe { jfn_mpv_set_property_flag_async(name.as_ptr(), v) };
}
unsafe fn set_double(name: &CStr, v: f64) {
    unsafe { jfn_mpv_set_property_double_async(name.as_ptr(), v) };
}
unsafe fn set_str(name: &CStr, v: &CStr) {
    unsafe { jfn_mpv_set_property_string_async(name.as_ptr(), v.as_ptr()) };
}

fn set_str_checked(name: &CStr, value: &CStr) -> crate::Result<()> {
    let handle = checked_raw()?;
    let mut value_ptr = value.as_ptr();
    crate::error::check(unsafe {
        sys::mpv_set_property(
            handle,
            name.as_ptr(),
            sys::mpv_format::MPV_FORMAT_STRING,
            &mut value_ptr as *mut _ as *mut c_void,
        )
    })
}

fn property_string_checked(name: &CStr) -> crate::Result<CString> {
    let handle = checked_raw()?;
    let value = unsafe { sys::mpv_get_property_string(handle, name.as_ptr()) };
    if value.is_null() {
        return Err(crate::Error::new(
            sys::mpv_error::MPV_ERROR_PROPERTY_UNAVAILABLE.0,
        ));
    }
    let copied = unsafe { CStr::from_ptr(value) }.to_owned();
    unsafe { sys::mpv_free(value.cast()) };
    Ok(copied)
}

fn cmd_checked(args: &[&CStr]) -> crate::Result<()> {
    let handle = checked_raw()?;
    let mut pointers: Vec<*const c_char> = args.iter().map(|value| value.as_ptr()).collect();
    pointers.push(std::ptr::null());
    crate::error::check(unsafe { sys::mpv_command(handle, pointers.as_mut_ptr()) })
}

fn cmd(args: &[&CStr]) {
    let ptrs: Vec<*const c_char> = args.iter().map(|s| s.as_ptr()).collect();
    unsafe { jfn_mpv_command_async(ptrs.as_ptr(), ptrs.len()) };
}

pub fn jfn_mpv_play() {
    unsafe { set_flag(c"pause", false) };
}
pub fn jfn_mpv_pause() {
    unsafe { set_flag(c"pause", true) };
}
pub fn jfn_mpv_toggle_pause() {
    cmd(&[c"cycle", c"pause"]);
}
pub fn jfn_mpv_stop() {
    cmd(&[c"stop"]);
}

pub fn jfn_mpv_stop_checked() -> crate::Result<()> {
    named_command_async("stop", &[])
}
pub fn jfn_mpv_seek_absolute(secs: f64) {
    let s = CString::new(format!("{}", secs)).unwrap_or_default();
    cmd(&[c"seek", &s, c"absolute"]);
}
pub fn jfn_mpv_set_volume(v: f64) {
    unsafe { set_double(c"volume", v) };
}
pub fn jfn_mpv_set_muted(v: bool) {
    unsafe { set_flag(c"mute", v) };
}
pub fn jfn_mpv_set_speed(v: f64) {
    unsafe { set_double(c"speed", v) };
}
pub fn jfn_mpv_set_audio_delay(s: f64) {
    unsafe { set_double(c"audio-delay", s) };
}
pub fn jfn_mpv_set_subtitle_delay(s: f64) {
    unsafe { set_double(c"sub-delay", s) };
}
pub fn jfn_mpv_set_subtitle_style(font_size: f64, subtitle_position: f64) {
    unsafe {
        set_double(c"sub-font-size", font_size);
        set_double(c"sub-pos", subtitle_position);
    }
}
pub fn jfn_mpv_set_start_position(s: f64) {
    unsafe { set_double(c"start", s) };
}

/// Track id sentinel: 0 = disabled. >=1 = explicit mpv track id.
/// Jellyfin is authoritative. Startup validates these ids before enabling
/// native selection; only unprobed audio explicitly delegates to mpv.
const TRACK_DISABLE: i64 = 0;

fn track_to_mpv_str(id: i64) -> CString {
    if id == TRACK_DISABLE {
        CString::new("no").unwrap_or_default()
    } else {
        CString::new(id.to_string()).unwrap_or_default()
    }
}

pub fn jfn_mpv_set_audio_track(id: i64) {
    let s = track_to_mpv_str(id);
    unsafe { set_str(c"aid", &s) };
}

pub fn jfn_mpv_set_audio_track_checked(id: i64) -> crate::Result<()> {
    let value = track_to_mpv_str(id);
    set_str_checked(c"aid", &value)
}

pub fn jfn_mpv_set_subtitle_track(id: i64) {
    let s = track_to_mpv_str(id);
    unsafe { set_str(c"sid", &s) };
}

pub fn jfn_mpv_set_subtitle_track_checked(id: i64) -> crate::Result<()> {
    let value = track_to_mpv_str(id);
    set_str_checked(c"sid", &value)
}

pub unsafe fn jfn_mpv_sub_add(url: *const c_char) {
    let Some(u) = (unsafe { cstr(url) }) else {
        return;
    };
    cmd(&[c"sub-add", u, c"select"]);
}

pub fn jfn_mpv_sub_add_checked(url: &CStr) -> crate::Result<()> {
    cmd_checked(&[c"sub-add", url, c"select"])
}

pub fn jfn_mpv_sub_remove_current() {
    cmd(&[c"sub-remove"]);
}

pub fn jfn_mpv_sub_remove_current_checked() -> crate::Result<()> {
    cmd_checked(&[c"sub-remove"])
}

/// Add an ASS danmaku track as the secondary subtitle, preserving the current
/// primary subtitle selection. The caller owns the returned external track ID.
pub fn jfn_mpv_danmaku_subtitle_add_checked(url: &CStr) -> crate::Result<i64> {
    cmd_checked(&[c"sub-add", url, c"auto", c"MediaStation Danmaku", c"und"])?;
    let id = danmaku_subtitle_track_id(None)?;
    set_str_checked(c"secondary-sid", &track_to_mpv_str(id))?;
    Ok(id)
}

/// Permit ASS positioning and animation tags on the secondary subtitle used by
/// danmaku. Returns the user's previous choice so the playback session can put
/// it back after its auxiliary track is removed.
pub fn jfn_mpv_danmaku_subtitle_enable_ass_tags_checked() -> crate::Result<CString> {
    let previous = property_string_checked(c"secondary-sub-ass-override")?;
    set_str_checked(c"secondary-sub-ass-override", c"no")?;
    Ok(previous)
}

/// Restore the user's secondary subtitle ASS override after danmaku stops.
pub fn jfn_mpv_danmaku_subtitle_restore_ass_tags_checked(previous: &CStr) -> crate::Result<()> {
    set_str_checked(c"secondary-sub-ass-override", previous)
}

/// Replace a danmaku ASS track after its file was rewritten. `sub-reload`
/// selects its replacement as primary, so load the replacement with `auto`,
/// switch only `secondary-sid`, then remove the old owned track.
pub fn jfn_mpv_danmaku_subtitle_reload_checked(id: i64, url: &CStr) -> crate::Result<i64> {
    cmd_checked(&[c"sub-add", url, c"auto", c"MediaStation Danmaku", c"und"])?;
    let replacement = danmaku_subtitle_track_id(Some(id))?;
    set_str_checked(c"secondary-sid", &track_to_mpv_str(replacement))?;
    let id = CString::new(id.to_string())
        .map_err(|_| crate::Error::new(sys::mpv_error::MPV_ERROR_INVALID_PARAMETER.0))?;
    cmd_checked(&[c"sub-remove", &id])?;
    Ok(replacement)
}

fn danmaku_subtitle_track_id(excluding: Option<i64>) -> crate::Result<i64> {
    jfn_mpv_get_property_node(c"track-list")?
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|track| {
            let id = track.get("id")?.as_int()?;
            (track.get("type")?.as_str() == Some("sub")
                && track.get("external")?.as_flag() == Some(true)
                && track.get("title")?.as_str() == Some("MediaStation Danmaku")
                && Some(id) != excluding)
                .then_some(id)
        })
        .max()
        .ok_or_else(|| crate::Error::new(sys::mpv_error::MPV_ERROR_INVALID_PARAMETER.0))
}

/// Show or hide the owned danmaku secondary subtitle without touching the
/// primary subtitle selected by the user.
pub fn jfn_mpv_danmaku_subtitle_set_enabled_checked(id: i64, enabled: bool) -> crate::Result<()> {
    if enabled {
        let track = track_to_mpv_str(id);
        set_str_checked(c"secondary-sid", &track)
    } else {
        set_str_checked(c"secondary-sid", c"no")
    }
}

/// Remove the owned danmaku subtitle track at the end of a playback session.
pub fn jfn_mpv_danmaku_subtitle_remove_checked(id: i64) -> crate::Result<()> {
    set_str_checked(c"secondary-sid", c"no")?;
    let id = CString::new(id.to_string())
        .map_err(|_| crate::Error::new(sys::mpv_error::MPV_ERROR_INVALID_PARAMETER.0))?;
    cmd_checked(&[c"sub-remove", &id])
}

pub unsafe fn jfn_mpv_audio_add(url: *const c_char) {
    let Some(u) = (unsafe { cstr(url) }) else {
        return;
    };
    cmd(&[c"audio-add", u, c"select"]);
}

// =============================================================================
// Danmaku OSD overlays (ass-events)
// =============================================================================

/// Push one danmaku overlay frame.
///
/// The caller recomputes positions every tick (`jfn-danmaku` does the layout)
/// and calls this with the assembled ASS event lines; `data = NULL` removes the
/// overlay again. Overlays live above mpv's builtin OSD, so subtitles are never
/// displaced by danmaku.
pub unsafe fn jfn_mpv_danmaku_overlay(
    id: i64,
    data: *const c_char,
    res_x: i32,
    res_y: i32,
    z: i32,
) {
    let text = if data.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(data) }.to_str().ok()
    };
    let args = overlay_arguments(id, text, res_x, res_y, z);
    if let Err(error) = named_command_async("osd-overlay", &args) {
        tracing::warn!(%error, "danmaku overlay command rejected");
    }
}

/// Remove a danmaku overlay (equivalent to pushing `format=none`).
pub fn jfn_mpv_danmaku_overlay_remove(id: i64) {
    let args = overlay_arguments(id, None, 0, 0, 0);
    if let Err(error) = named_command_async("osd-overlay", &args) {
        tracing::warn!(%error, "danmaku overlay removal rejected");
    }
}

fn named_command_async(name: &str, args: &[(String, String)]) -> crate::Result<()> {
    // libmpv's MPV_ERROR_INVALID_PARAMETER; only reachable if a command
    // argument contains an interior NUL (danmaku text is player content).
    const MPV_ERROR_INVALID_PARAMETER: i32 = -4;
    let handle = checked_raw()?;
    let mut command = NamedArgsCommand::new(name, args)
        .map_err(|_| crate::Error::new(MPV_ERROR_INVALID_PARAMETER))?;
    crate::error::check(unsafe { sys::mpv_command_node_async(handle, 0, command.as_mut_ptr()) })
}

// =============================================================================
// LoadFile + deferred track selection (stateful)
// =============================================================================

/// Load options for `LoadFile`. NULL string pointers are treated as empty.
#[repr(C)]
pub struct JfnMpvLoadOptions {
    pub start_secs: f64,
    pub video_track: i64,
    pub audio_track: i64,
    pub sub_track: i64,
    pub external_audio_url: *const c_char,
    pub external_sub_url: *const c_char,
    /// Comma-separated mpv `http-header-fields` string-list value.
    pub http_header_fields: *const c_char,
    /// Per-file HTTP proxy URL. Empty keeps mpv's direct/default path.
    pub http_proxy: *const c_char,
    /// Per-file video filter chain. Empty preserves the normal player path.
    pub video_filter: *const c_char,
    /// Per-file hardware decoder mode. Empty preserves the configured mode.
    pub hwdec: *const c_char,
    /// VFG presentation follows display vblanks without changing audio speed.
    /// Per-file: mpv restores the configured video sync on the next load.
    pub vfg_display_sync: bool,
    /// Apply the MediaStation text-subtitle style for this file only.
    pub subtitle_style_override: bool,
    pub subtitle_font_size: f64,
    pub subtitle_position: f64,
    /// Let mpv select the container's default audio track for this file.
    pub defer_audio_to_mpv: bool,
}

#[derive(Debug)]
pub enum LoadError {
    HandleUnavailable,
    InvalidArgument(&'static str),
    InvalidOption(std::ffi::NulError),
    Command(crate::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HandleUnavailable => formatter.write_str("mpv handle is unavailable"),
            Self::InvalidArgument(name) => write!(formatter, "invalid load argument: {name}"),
            Self::InvalidOption(error) => write!(formatter, "invalid load option: {error}"),
            Self::Command(error) => write!(formatter, "mpv rejected loadfile: {error}"),
        }
    }
}

impl std::error::Error for LoadError {}

struct PendingTrack {
    path: String,
    vid: i64,
    aid: i64,
    sid: i64,
    external_audio_url: String,
    external_sub_url: String,
    defer_audio_to_mpv: bool,
    prepared: bool,
    valid: bool,
}

fn pending_slot() -> &'static Mutex<PendingTrack> {
    use std::sync::OnceLock;
    static SLOT: OnceLock<Mutex<PendingTrack>> = OnceLock::new();
    SLOT.get_or_init(|| {
        Mutex::new(PendingTrack {
            path: String::new(),
            vid: 1,
            aid: TRACK_DISABLE,
            sid: TRACK_DISABLE,
            external_audio_url: String::new(),
            external_sub_url: String::new(),
            defer_audio_to_mpv: false,
            prepared: false,
            valid: false,
        })
    })
}

unsafe fn cstr_to_string(p: *const c_char) -> String {
    unsafe { cstr(p) }
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn media_station_subtitle_options(font_size: f64, subtitle_position: f64) -> Vec<(String, String)> {
    vec![
        ("sub-ass-override".to_string(), "force".to_string()),
        ("sub-font".to_string(), "Microsoft YaHei UI".to_string()),
        ("sub-font-size".to_string(), font_size.to_string()),
        ("sub-color".to_string(), "#FFFFFF".to_string()),
        ("sub-outline-color".to_string(), "#C0000000".to_string()),
        ("sub-outline-size".to_string(), "2".to_string()),
        ("sub-shadow-offset".to_string(), "0".to_string()),
        ("sub-pos".to_string(), subtitle_position.to_string()),
        ("sub-align-x".to_string(), "center".to_string()),
        ("sub-align-y".to_string(), "bottom".to_string()),
        ("sub-justify".to_string(), "center".to_string()),
    ]
}

fn startup_position_option(seconds: f64) -> String {
    if seconds == 0.0 {
        "none".to_string()
    } else {
        seconds.to_string()
    }
}

pub unsafe fn jfn_mpv_load_file(
    path: *const c_char,
    opts: *const JfnMpvLoadOptions,
) -> Result<(), LoadError> {
    unsafe { jfn_mpv_load_file_with_initial_head(path, opts, std::sync::Arc::from([])) }
}

/// Load with a validated prefix from this URL's CDN validation response.
/// The prefix is copied by Arc into the queued stream, never into its URI.
///
/// # Safety
/// `path` and `opts` follow the same lifetime requirements as `jfn_mpv_load_file`.
pub unsafe fn jfn_mpv_load_file_with_initial_head(
    path: *const c_char,
    opts: *const JfnMpvLoadOptions,
    initial_head: std::sync::Arc<[u8]>,
) -> Result<(), LoadError> {
    let Some(path_c) = (unsafe { cstr(path) }) else {
        return Err(LoadError::InvalidArgument("path"));
    };
    let Some(o) = (unsafe { opts.as_ref() }) else {
        return Err(LoadError::InvalidArgument("options"));
    };

    let ext_audio = unsafe { cstr_to_string(o.external_audio_url) };
    let ext_sub = unsafe { cstr_to_string(o.external_sub_url) };
    let http_header_fields = unsafe { cstr_to_string(o.http_header_fields) };
    let http_proxy = unsafe { cstr_to_string(o.http_proxy) };
    let video_filter = unsafe { cstr_to_string(o.video_filter) };
    let hwdec = unsafe { cstr_to_string(o.hwdec) };
    let defer_audio =
        should_defer_audio_to_mpv(o.defer_audio_to_mpv, o.audio_track, !ext_audio.is_empty());

    let mut load_options = vec![
        ("start".to_string(), startup_position_option(o.start_secs)),
        ("pause".to_string(), "yes".to_string()),
        // The on_preloaded hook supplies the final selectors before mpv
        // initializes decoders and executes the initial resume seek.
        ("track-auto-selection".to_string(), "yes".to_string()),
        ("vid".to_string(), "no".to_string()),
        ("aid".to_string(), "no".to_string()),
        ("sid".to_string(), "no".to_string()),
    ];
    if o.subtitle_style_override {
        load_options.extend(media_station_subtitle_options(
            o.subtitle_font_size,
            o.subtitle_position,
        ));
    }
    if !http_header_fields.is_empty() {
        load_options.push(("http-header-fields".to_string(), http_header_fields));
    }
    if !http_proxy.is_empty() {
        load_options.push(("http-proxy".to_string(), http_proxy));
    }
    if !video_filter.is_empty() {
        load_options.push(("vf".to_string(), video_filter));
    }
    if !hwdec.is_empty() {
        load_options.push(("hwdec".to_string(), hwdec));
    }
    if o.vfg_display_sync {
        load_options.push(("video-sync".to_string(), "display-vdrop".to_string()));
    }

    let mut command =
        LoadFileCommand::new(path_c, &load_options).map_err(LoadError::InvalidOption)?;
    let handle = raw();
    if handle.is_null() {
        return Err(LoadError::HandleUnavailable);
    }

    crate::stream_cb::prepare_initial_head(&path_c.to_string_lossy(), initial_head)
        .map_err(LoadError::InvalidArgument)?;

    // Keep a single authoritative selection, including the subtitle resolved
    // from the parsed local catalog by the preloaded action sink.
    {
        let mut s = pending_slot().lock();
        s.path = path_c.to_string_lossy().into_owned();
        s.vid = o.video_track;
        s.aid = o.audio_track;
        s.sid = o.sub_track;
        s.external_audio_url = ext_audio;
        s.external_sub_url = ext_sub;
        s.defer_audio_to_mpv = defer_audio;
        s.prepared = false;
        s.valid = true;
    }
    let result = unsafe { sys::mpv_command_node_async(handle, 0, command.as_mut_ptr()) };
    if let Err(error) = crate::error::check(result) {
        pending_slot().lock().valid = false;
        crate::stream_cb::discard_initial_head(&path_c.to_string_lossy());
        return Err(LoadError::Command(error));
    }
    Ok(())
}

fn should_defer_audio_to_mpv(requested: bool, audio_track: i64, has_external_audio: bool) -> bool {
    requested && audio_track == TRACK_DISABLE && !has_external_audio
}

fn set_pending_embedded_subtitle(track: &mut PendingTrack, id: i64) -> bool {
    if !track.valid || !track.external_sub_url.is_empty() || id <= 0 {
        return false;
    }
    track.sid = id;
    true
}

/// Resolve a container subtitle in on_preloaded, before native track init.
/// External subtitles retain the per-load path prepared by the caller.
pub fn jfn_mpv_set_pending_embedded_subtitle(id: i64) -> bool {
    set_pending_embedded_subtitle(&mut pending_slot().lock(), id)
}

fn current_path_matches(handle: *mut sys::mpv_handle, pending: &PendingTrack) -> bool {
    if !pending.valid {
        return false;
    }
    let path = unsafe { sys::mpv_get_property_string(handle, c"path".as_ptr()) };
    if path.is_null() {
        return false;
    }
    let matches = unsafe { CStr::from_ptr(path) }.to_bytes() == pending.path.as_bytes();
    unsafe { sys::mpv_free(path.cast()) };
    matches
}

pub fn jfn_mpv_pending_tracks_match_current_file() -> bool {
    let Ok(handle) = checked_raw() else {
        return false;
    };
    current_path_matches(handle, &pending_slot().lock())
}

fn startup_selector(catalog: &crate::Node, kind: &str, id: i64) -> Result<String, String> {
    if id == TRACK_DISABLE {
        return Ok("no".to_string());
    }
    let tracks = catalog
        .as_array()
        .ok_or("native track catalog is unavailable")?;
    let mut has_kind = false;
    for track in tracks {
        if track.get("type").and_then(crate::Node::as_str) == Some(kind) {
            has_kind = true;
            if track.get("id").and_then(crate::Node::as_int) == Some(id) {
                return Ok(id.to_string());
            }
        }
    }
    // An audio-only file has no video; a video-only file has no audio.
    if !has_kind {
        return Ok("no".to_string());
    }
    Err(format!("requested {kind} track {id} is unavailable"))
}

fn prepare_tracks(handle: *mut sys::mpv_handle, pending: &PendingTrack) -> Result<bool, String> {
    let mut node: sys::mpv_node = unsafe { std::mem::zeroed() };
    crate::error::check(unsafe {
        sys::mpv_get_property(
            handle,
            c"track-list".as_ptr(),
            sys::mpv_format::MPV_FORMAT_NODE,
            (&mut node as *mut sys::mpv_node).cast(),
        )
    })
    .map_err(|e| format!("read startup track catalog: {e}"))?;
    let catalog = unsafe { crate::Node::from_raw(&node) };
    unsafe { sys::mpv_free_node_contents(&mut node) };
    let video = startup_selector(&catalog, "video", pending.vid)?;
    let has_video = video != "no";
    let audio = if !pending.external_audio_url.is_empty() {
        "no".to_string()
    } else if pending.defer_audio_to_mpv {
        "auto".to_string()
    } else {
        startup_selector(&catalog, "audio", pending.aid)?
    };
    let sub = if pending.external_sub_url.is_empty() {
        startup_selector(&catalog, "sub", pending.sid)?
    } else {
        "no".to_string()
    };
    for (name, value) in [
        (c"file-local-options/vid", video),
        (c"file-local-options/aid", audio),
        (c"file-local-options/sid", sub),
    ] {
        let value = CString::new(value).map_err(|e| e.to_string())?;
        crate::error::check(unsafe {
            sys::mpv_set_property_string(handle, name.as_ptr(), value.as_ptr())
        })
        .map_err(|e| format!("set startup {}: {e}", name.to_string_lossy()))?;
    }
    // Before playback_initialized, mpv adds external tracks and marks the
    // selector without switching/reinitializing an already running chain.
    for (command, path) in [
        ("audio-add", &pending.external_audio_url),
        ("sub-add", &pending.external_sub_url),
    ] {
        if !path.is_empty() {
            let args = crate::Command::new([command, path, "select"])
                .map_err(|_| format!("invalid startup {command} path"))?;
            crate::error::check(unsafe { sys::mpv_command(handle, args.as_ptr()) })
                .map_err(|e| format!("startup {command} failed: {e}"))?;
        }
    }
    Ok(has_video)
}

/// Called on the coordinator worker while on_preloaded suspends native load.
/// Never call this from a stream callback or the mpv wakeup callback.
pub fn jfn_mpv_prepare_pending_tracks() -> Result<Option<bool>, String> {
    let handle = checked_raw().map_err(|e| e.to_string())?;
    let mut pending = pending_slot().lock();
    if !current_path_matches(handle, &pending) {
        return Ok(None); // An old hook may arrive while a replacement load is queued.
    }
    let started = std::time::Instant::now();
    match prepare_tracks(handle, &pending) {
        Ok(has_video) => {
            pending.prepared = true;
            tracing::info!(target: "mpv", "startup tracks prepared before decoder init: elapsed_ms={}", started.elapsed().as_millis());
            Ok(Some(has_video))
        }
        Err(error) => {
            pending.valid = false;
            Err(error)
        }
    }
}

pub fn jfn_mpv_continue_preloaded(hook_id: u64) -> crate::Result<()> {
    crate::error::check(unsafe { sys::mpv_hook_continue(checked_raw()?, hook_id) })
}

pub fn jfn_mpv_apply_pending_track_selection_and_play() -> Result<(), String> {
    let handle = checked_raw().map_err(|e| e.to_string())?;
    let mut pending = pending_slot().lock();
    if !current_path_matches(handle, &pending) {
        return Ok(());
    }
    if !pending.prepared {
        return Err("startup tracks were not prepared before file-loaded".to_string());
    }
    pending.valid = false;
    set_str_checked(c"pause", c"no").map_err(|e| e.to_string())
}

// =============================================================================
// Aspect-mode helper
// =============================================================================

pub unsafe fn jfn_mpv_set_aspect_mode(mode: *const c_char) {
    let Some(m) = (unsafe { cstr(mode) }) else {
        return;
    };
    let (keepaspect, panscan) = match m.to_bytes() {
        b"auto" => (true, 0.0),
        b"cover" => (true, 1.0),
        b"fill" => (false, 0.0),
        _ => {
            // Unknown mode — silently ignore (matches legacy log-and-skip).
            return;
        }
    };
    unsafe { set_flag(c"keepaspect", keepaspect) };
    unsafe { set_double(c"panscan", panscan) };
}

// =============================================================================
// Window / display
// =============================================================================

pub fn jfn_mpv_set_fullscreen(v: bool) {
    unsafe { set_flag(c"fullscreen", v) };
}
pub fn jfn_mpv_toggle_fullscreen() {
    cmd(&[c"cycle", c"fullscreen"]);
}
pub fn jfn_mpv_set_window_minimized(v: bool) {
    unsafe { set_flag(c"window-minimized", v) };
}
pub fn jfn_mpv_set_window_maximized(v: bool) {
    unsafe { set_flag(c"window-maximized", v) };
}
pub fn jfn_mpv_set_force_window_position(v: bool) {
    unsafe { set_flag(c"force-window-position", v) };
}
pub unsafe fn jfn_mpv_set_geometry(g: *const c_char) {
    let Some(g) = (unsafe { cstr(g) }) else {
        return;
    };
    unsafe { set_str(c"geometry", g) };
}

/// Returns the parsed packed RGB color of mpv's `background-color`
/// property (0x00RRGGBB), or 0 if the property is unavailable or
/// malformed.
pub fn jfn_mpv_get_background_color() -> u32 {
    let p = unsafe { jfn_mpv_get_property_string(c"background-color".as_ptr()) };
    if p.is_null() {
        return 0;
    }
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    let rgb = crate::color::parse(&s);
    unsafe { jfn_mpv_free_string(p) };
    rgb
}

pub unsafe fn jfn_mpv_set_background_color_hex(hex: *const c_char) {
    let Some(h) = (unsafe { cstr(hex) }) else {
        return;
    };
    unsafe { set_str(c"background-color", h) };
}

#[cfg(test)]
mod tests {
    use super::{
        PendingTrack, media_station_subtitle_options, set_pending_embedded_subtitle,
        should_defer_audio_to_mpv,
    };

    #[test]
    fn loaded_subtitle_updates_the_pending_selector_before_playback() {
        let mut pending = PendingTrack {
            path: "fixture.mkv".to_string(),
            vid: 1,
            aid: 2,
            sid: 0,
            external_audio_url: String::new(),
            external_sub_url: String::new(),
            defer_audio_to_mpv: false,
            prepared: false,
            valid: true,
        };
        assert!(set_pending_embedded_subtitle(&mut pending, 7));
        assert_eq!(pending.sid, 7);
        assert_eq!(pending.aid, 2);
        assert!(!set_pending_embedded_subtitle(&mut pending, 0));
        pending.external_sub_url = "prepared.srt".to_string();
        assert!(!set_pending_embedded_subtitle(&mut pending, 8));
        assert_eq!(pending.sid, 7);
        pending.external_sub_url.clear();
        pending.valid = false;
        assert!(!set_pending_embedded_subtitle(&mut pending, 8));
    }

    #[test]
    fn media_station_subtitle_style_uses_vertical_position_not_layout_margin() {
        let options = media_station_subtitle_options(36.0, 92.0);

        assert!(options.contains(&("sub-font-size".to_string(), "36".to_string())));
        assert!(options.contains(&("sub-pos".to_string(), "92".to_string())));
        assert!(!options.iter().any(|(name, _)| name == "sub-margin-y"));
    }

    #[test]
    fn deferred_audio_requires_an_unselected_internal_track() {
        assert!(should_defer_audio_to_mpv(true, 0, false));
        assert!(!should_defer_audio_to_mpv(false, 0, false));
        assert!(!should_defer_audio_to_mpv(true, 1, false));
        assert!(!should_defer_audio_to_mpv(true, 0, true));
    }

    #[test]
    fn startup_selection_keeps_explicit_ids_and_never_falls_back_to_another_track() {
        use crate::Node;
        let catalog = Node::Array(vec![Node::Map(vec![
            ("type".into(), Node::String("audio".into())),
            ("id".into(), Node::Int(2)),
        ])]);
        assert_eq!(super::startup_selector(&catalog, "audio", 2).unwrap(), "2");
        assert_eq!(super::startup_selector(&catalog, "audio", 0).unwrap(), "no");
        assert!(super::startup_selector(&catalog, "audio", 1).is_err());
        assert_eq!(super::startup_selector(&catalog, "video", 1).unwrap(), "no");
    }

    #[test]
    #[ignore = "requires installed libmpv and MPV_STREAM_TEST_FIXTURE local video"]
    fn native_preloaded_selection_avoids_post_load_switch_and_preserves_resume()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{Command, Event, Handle, LogLevel, PropertyValue};
        use std::ffi::CString;
        use std::time::{Duration, Instant};

        let fixture = std::env::var("MPV_STREAM_TEST_FIXTURE")?;
        let path = CString::new(fixture.clone())?;
        struct TestSubtitle(std::path::PathBuf);
        impl Drop for TestSubtitle {
            fn drop(&mut self) {
                if let Err(error) = std::fs::remove_file(&self.0) {
                    eprintln!("test subtitle cleanup failed: {error}");
                }
            }
        }
        let subtitle_path =
            std::env::temp_dir().join(format!("jellium-startup-sub-{}.srt", std::process::id()));
        let mut subtitle_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&subtitle_path)?;
        let subtitle = TestSubtitle(subtitle_path);
        std::io::Write::write_all(
            &mut subtitle_file,
            b"1\n00:00:00,000 --> 00:00:00,900\nStartup subtitle\n",
        )?;
        drop(subtitle_file);
        for (preloaded, start, external_sub) in [
            (false, 0.4, false),
            (true, 0.4, false),
            (true, 0.0, false),
            (true, 0.4, true),
        ] {
            let handle = Handle::create()?;
            for (name, value) in [
                ("config", "no"),
                ("vo", "null"),
                ("ao", "null"),
                ("terminal", "no"),
                ("hwdec", "no"),
                ("track-auto-selection", "no"),
                ("cache", "no"),
                ("demuxer-readahead-secs", "0"),
            ] {
                handle.set_option_string(name, value)?;
            }
            handle.initialize()?;
            handle.request_log_messages(LogLevel::Info)?;
            handle.observe_property_node(1, "video-frame-info")?;
            if preloaded {
                crate::error::check(unsafe {
                    crate::sys::mpv_hook_add(handle.raw(), 0, c"on_preloaded".as_ptr(), 0)
                })?;
            }
            let mut options = vec![
                (
                    "start".to_string(),
                    if preloaded {
                        super::startup_position_option(start)
                    } else {
                        start.to_string()
                    },
                ),
                ("pause".to_string(), "yes".to_string()),
            ];
            if preloaded {
                options.extend([
                    ("track-auto-selection".to_string(), "yes".to_string()),
                    ("vid".to_string(), "no".to_string()),
                    ("aid".to_string(), "no".to_string()),
                    ("sid".to_string(), "no".to_string()),
                ]);
            }
            let mut load = super::LoadFileCommand::new(&path, &options)?;
            let started = Instant::now();
            crate::error::check(unsafe {
                crate::sys::mpv_command_node_async(handle.raw(), 0, load.as_mut_ptr())
            })?;
            let mut switches = 0;
            let mut seeks = 0;
            let mut first_frame = false;
            let mut native_started = false;
            let mut loaded = false;
            let mut prepared = false;
            while started.elapsed() < Duration::from_secs(5) {
                match handle.wait_event(0.01) {
                    Event::Hook { reply, name } if name == "on_preloaded" => {
                        let pending = PendingTrack {
                            path: fixture.clone(),
                            vid: 1,
                            aid: 0,
                            sid: 0,
                            external_audio_url: String::new(),
                            external_sub_url: if external_sub {
                                subtitle
                                    .0
                                    .to_str()
                                    .ok_or("invalid local subtitle path")?
                                    .to_string()
                            } else {
                                String::new()
                            },
                            defer_audio_to_mpv: false,
                            prepared: false,
                            valid: true,
                        };
                        assert!(super::current_path_matches(handle.raw(), &pending));
                        assert!(super::prepare_tracks(handle.raw(), &pending)?);
                        prepared = true;
                        crate::error::check(unsafe {
                            crate::sys::mpv_hook_continue(handle.raw(), reply)
                        })?;
                    }
                    Event::FileLoaded => {
                        loaded = true;
                        if preloaded {
                            assert!(prepared);
                            assert_eq!(handle.get_property::<i64>("vid")?, 1);
                            if external_sub {
                                assert_eq!(handle.get_property::<i64>("sid")?, 1);
                            } else {
                                assert_eq!(handle.get_property_string("sid")?, "no");
                            }
                        } else {
                            assert!(handle.get_property_string("vid")? == "no");
                            handle.set_property_string_async(0, "vid", "1")?;
                            handle.set_property_string_async(0, "aid", "no")?;
                            handle.set_property_string_async(0, "sid", "no")?;
                        }
                        handle.set_property_flag_async(0, "pause", false)?;
                    }
                    Event::Seek => seeks += 1,
                    Event::PlaybackRestart => native_started = true,
                    Event::PropertyChange { id: 1, value, .. } if value != PropertyValue::None => {
                        first_frame = true
                    }
                    Event::LogMessage(message) if message.text.contains("Track switched:") => {
                        switches += 1
                    }
                    Event::EndFile(reason) => {
                        return Err(format!("fixture ended before readiness: {reason:?}").into());
                    }
                    _ => {}
                }
                if first_frame && native_started {
                    break;
                }
            }
            assert!(
                loaded && first_frame && native_started,
                "local video never reached readiness"
            );
            let position = handle.get_property::<f64>("time-pos")?;
            eprintln!(
                "preloaded={preloaded} start={start}: switches={switches} seeks={seeks} video_ready={:?} position={position}",
                started.elapsed()
            );
            if preloaded {
                assert!(
                    (position - start).abs() < 0.2,
                    "resume position {position}, expected {start}"
                );
                assert_eq!(switches, 0);
                assert_eq!(seeks, usize::from(start != 0.0));
            } else {
                assert!(switches >= 1);
            }
            handle.command(&Command::new(["stop"])?)?;
        }
        Ok(())
    }
}
