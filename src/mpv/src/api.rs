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

use crate::{command::LoadFileCommand, sys};

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
/// Mpv's auto-track-selection is globally disabled (boot applies
/// `track-auto-selection=no`); jellyfin-web is the authority.
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

pub unsafe fn jfn_mpv_audio_add(url: *const c_char) {
    let Some(u) = (unsafe { cstr(url) }) else {
        return;
    };
    cmd(&[c"audio-add", u, c"select"]);
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
    vid: i64,
    aid: i64,
    sid: i64,
    external_audio_url: String,
    external_sub_url: String,
    defer_audio_to_mpv: bool,
    valid: bool,
}

fn pending_slot() -> &'static Mutex<PendingTrack> {
    use std::sync::OnceLock;
    static SLOT: OnceLock<Mutex<PendingTrack>> = OnceLock::new();
    SLOT.get_or_init(|| {
        Mutex::new(PendingTrack {
            vid: 1,
            aid: TRACK_DISABLE,
            sid: TRACK_DISABLE,
            external_audio_url: String::new(),
            external_sub_url: String::new(),
            defer_audio_to_mpv: false,
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

pub unsafe fn jfn_mpv_load_file(
    path: *const c_char,
    opts: *const JfnMpvLoadOptions,
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
        ("start".to_string(), o.start_secs.to_string()),
        ("pause".to_string(), "yes".to_string()),
    ];
    if o.subtitle_style_override {
        load_options.extend(media_station_subtitle_options(
            o.subtitle_font_size,
            o.subtitle_position,
        ));
    }
    if defer_audio {
        // Per-file enable so mpv's demuxer picks the format-correct
        // audio track (HLS DEFAULT=YES, MPEG-TS first PMT, etc.). We
        // explicitly write `sid=no` after FILE_LOADED to keep subs off.
        load_options.push(("track-auto-selection".to_string(), "yes".to_string()));
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

    let mut command =
        LoadFileCommand::new(path_c, &load_options).map_err(LoadError::InvalidOption)?;
    let handle = raw();
    if handle.is_null() {
        return Err(LoadError::HandleUnavailable);
    }

    // Track selection is owned by Jellyfin. With track-auto-selection=no,
    // mpv silently drops aid/vid/sid in loadfile options (loadfile.c
    // skips select_default_track entirely). Load the file *paused* with
    // no selectors, stash the intended ids, and apply them via property
    // writes after FILE_LOADED. The async writes + final pause=false are
    // FIFO-ordered on mpv's core thread, so playback only begins after
    // track-switch reinits land.
    {
        let mut s = pending_slot().lock();
        s.vid = o.video_track;
        s.aid = o.audio_track;
        s.sid = o.sub_track;
        s.external_audio_url = ext_audio;
        s.external_sub_url = ext_sub;
        s.defer_audio_to_mpv = defer_audio;
        s.valid = true;
    }
    let result = unsafe { sys::mpv_command_node_async(handle, 0, command.as_mut_ptr()) };
    if let Err(error) = crate::error::check(result) {
        pending_slot().lock().valid = false;
        return Err(LoadError::Command(error));
    }
    Ok(())
}

fn should_defer_audio_to_mpv(requested: bool, audio_track: i64, has_external_audio: bool) -> bool {
    requested && audio_track == TRACK_DISABLE && !has_external_audio
}

pub fn jfn_mpv_apply_pending_track_selection_and_play() {
    let snapshot = {
        let mut s = pending_slot().lock();
        if !s.valid {
            return;
        }
        let snap = (
            s.vid,
            s.aid,
            s.sid,
            std::mem::take(&mut s.external_audio_url),
            std::mem::take(&mut s.external_sub_url),
            s.defer_audio_to_mpv,
        );
        s.valid = false;
        s.defer_audio_to_mpv = false;
        snap
    };
    let (vid, aid, sid, ext_audio, ext_sub, defer_audio) = snapshot;

    let vid_s = track_to_mpv_str(vid);
    unsafe { set_str(c"vid", &vid_s) };
    if !defer_audio {
        // Normal path: the server catalog is authoritative. Skipped only
        // when an unprobed source explicitly delegates the initial audio
        // choice to mpv's demuxer for this file.
        let aid_s = track_to_mpv_str(aid);
        unsafe { set_str(c"aid", &aid_s) };
    }
    let sid_s = track_to_mpv_str(sid);
    unsafe { set_str(c"sid", &sid_s) };
    if !ext_audio.is_empty() {
        match CString::new(ext_audio) {
            Ok(u) => cmd(&[c"audio-add", &u, c"select"]),
            Err(e) => tracing::warn!("ext audio path has interior NUL: {e}"),
        }
    }
    if !ext_sub.is_empty() {
        match CString::new(ext_sub) {
            Ok(u) => cmd(&[c"sub-add", &u, c"select"]),
            Err(e) => tracing::warn!("ext sub path has interior NUL: {e}"),
        }
    }
    unsafe { set_flag(c"pause", false) };
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
    use super::{media_station_subtitle_options, should_defer_audio_to_mpv};

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
}
