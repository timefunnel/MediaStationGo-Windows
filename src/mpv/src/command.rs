//! Helper for assembling NUL-terminated command argument arrays.
//!
//! libmpv's `mpv_command` / `mpv_command_async` take a `const char*[]`
//! terminated by a null pointer. This module owns the `CString` storage so
//! the pointers remain valid until the call returns.

use crate::sys;
use std::ffi::{CStr, CString, NulError};
use std::os::raw::c_char;

/// Owned command argument vector. Borrow `as_ptrs()` to obtain the
/// null-terminated pointer array libmpv expects.
pub struct Command {
    storage: Vec<CString>,
    ptrs: Vec<*const c_char>,
}

impl Command {
    pub fn new<I, S>(args: I) -> Result<Self, NulError>
    where
        I: IntoIterator<Item = S>,
        S: Into<Vec<u8>>,
    {
        let storage: Vec<CString> = args
            .into_iter()
            .map(|s| CString::new(s.into()))
            .collect::<Result<_, _>>()?;
        let mut ptrs: Vec<*const c_char> = storage.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        Ok(Self { storage, ptrs })
    }

    pub fn as_ptr(&self) -> *mut *const c_char {
        self.ptrs.as_ptr() as *mut _
    }

    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }
}

// SAFETY: `Command` is logically a `Vec<CString>` plus its derived pointer
// table. Both halves are owned by the struct. The raw pointers in `ptrs`
// point into `storage`, which has stable addresses because `CString::as_ptr`
// refers to heap memory not the `CString` itself. Moving the `Command`
// preserves those addresses.
unsafe impl Send for Command {}
unsafe impl Sync for Command {}

/// Owned `loadfile` command tree for `mpv_command_node_async`.
///
/// The backing strings, node arrays, and key arrays must outlive the API call.
/// They are retained as fields even though only `root` is accessed directly.
pub(crate) struct LoadFileCommand {
    _strings: Vec<CString>,
    _option_keys: Vec<CString>,
    _option_values: Vec<CString>,
    _option_nodes: Vec<sys::mpv_node>,
    _option_key_ptrs: Vec<*mut c_char>,
    _option_list: Box<sys::mpv_node_list>,
    _argument_nodes: Vec<sys::mpv_node>,
    _argument_list: Box<sys::mpv_node_list>,
    root: sys::mpv_node,
}

impl LoadFileCommand {
    pub(crate) fn new(path: &CStr, options: &[(String, String)]) -> Result<Self, NulError> {
        let strings = vec![
            CString::new("loadfile")?,
            path.to_owned(),
            CString::new("replace")?,
        ];
        let option_keys = options
            .iter()
            .map(|(name, _)| CString::new(name.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        let option_values = options
            .iter()
            .map(|(_, value)| CString::new(value.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut option_nodes: Vec<sys::mpv_node> = option_values
            .iter()
            .map(|value| string_node(value))
            .collect();
        let mut option_key_ptrs = option_keys
            .iter()
            .map(|key| key.as_ptr().cast_mut())
            .collect::<Vec<_>>();
        let mut option_list = Box::new(sys::mpv_node_list {
            num: option_nodes.len() as i32,
            values: option_nodes.as_mut_ptr(),
            keys: option_key_ptrs.as_mut_ptr(),
        });
        let mut argument_nodes = vec![
            string_node(&strings[0]),
            string_node(&strings[1]),
            string_node(&strings[2]),
            int_node(-1),
            list_node(sys::mpv_format::MPV_FORMAT_NODE_MAP, option_list.as_mut()),
        ];
        let mut argument_list = Box::new(sys::mpv_node_list {
            num: argument_nodes.len() as i32,
            values: argument_nodes.as_mut_ptr(),
            keys: std::ptr::null_mut(),
        });
        let root = list_node(
            sys::mpv_format::MPV_FORMAT_NODE_ARRAY,
            argument_list.as_mut(),
        );
        Ok(Self {
            _strings: strings,
            _option_keys: option_keys,
            _option_values: option_values,
            _option_nodes: option_nodes,
            _option_key_ptrs: option_key_ptrs,
            _option_list: option_list,
            _argument_nodes: argument_nodes,
            _argument_list: argument_list,
            root,
        })
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut sys::mpv_node {
        &mut self.root
    }
}

fn empty_node() -> sys::mpv_node {
    unsafe { std::mem::zeroed() }
}
fn string_node(value: &CStr) -> sys::mpv_node {
    let mut node = empty_node();
    node.format = sys::mpv_format::MPV_FORMAT_STRING;
    node.u.string = value.as_ptr().cast_mut();
    node
}

fn int_node(value: i64) -> sys::mpv_node {
    let mut node = empty_node();
    node.format = sys::mpv_format::MPV_FORMAT_INT64;
    node.u.int64 = value;
    node
}

fn list_node(format: sys::mpv_format, list: &mut sys::mpv_node_list) -> sys::mpv_node {
    let mut node = empty_node();
    node.format = format;
    node.u.list = list;
    node
}

// SAFETY: all raw pointers refer to owned heap allocations retained by the
// command. libmpv only reads the tree during the command API call.
unsafe impl Send for LoadFileCommand {}

/// Named arguments for `osd-overlay`.
///
/// mpv documents that `osd-overlay` should be called with named arguments
/// because `res_x`/`res_y`/`z`/`hidden`/`compute_bounds` are all optional;
/// passing `format = none` removes the overlay again. `data` is a *required*
/// argument (`finish_cmd()` in input/cmd.c rejects the command without it), so a
/// removal sends it empty — the docs' "mostly ignored" for `format = none` does
/// not extend to omitting the field.
pub(crate) fn overlay_arguments(
    id: i64,
    data: Option<&str>,
    res_x: i32,
    res_y: i32,
    z: i32,
) -> Vec<(String, String)> {
    match data {
        Some(text) => vec![
            ("id".to_string(), id.to_string()),
            ("format".to_string(), "ass-events".to_string()),
            ("data".to_string(), text.to_string()),
            ("res_x".to_string(), res_x.to_string()),
            ("res_y".to_string(), res_y.to_string()),
            ("z".to_string(), z.to_string()),
        ],
        None => vec![
            ("id".to_string(), id.to_string()),
            ("format".to_string(), "none".to_string()),
            ("data".to_string(), String::new()),
        ],
    }
}

/// Owned command tree for a named-argument call made through
/// `mpv_command_node_async`. Same lifetime rules as [`LoadFileCommand`].
///
/// mpv's named-argument form *is* the map: `mpv_command_node()` takes
/// `MPV_FORMAT_NODE_MAP` as the root, with the command name in a `_name` string
/// field (DOCS/man/input.rst, "Named arguments"; DOCS/man/ipc.rst, "Commands
/// with named arguments"). An array root is parsed positionally instead, so
/// `["osd-overlay", {..}]` is handed to the first declared argument and mpv
/// rejects it with "argument id has incompatible type".
pub(crate) struct NamedArgsCommand {
    // Kept alive so the pointers inside `root` stay valid; same convention as
    // `LoadFileCommand` above.
    _keys: Vec<CString>,
    _values: Vec<CString>,
    _nodes: Vec<sys::mpv_node>,
    _key_ptrs: Vec<*mut c_char>,
    _map: Box<sys::mpv_node_list>,
    root: sys::mpv_node,
}

impl NamedArgsCommand {
    pub(crate) fn new(name: &str, args: &[(String, String)]) -> Result<Self, NulError> {
        // The command name takes part in the map as `_name`.
        let mut keys: Vec<CString> = vec![CString::new("_name")?];
        let mut values: Vec<CString> = vec![CString::new(name)?];
        for (key, value) in args {
            keys.push(CString::new(key.as_str())?);
            values.push(CString::new(value.as_str())?);
        }
        let mut nodes: Vec<sys::mpv_node> = values.iter().map(|v| string_node(v)).collect();
        let mut key_ptrs = keys
            .iter()
            .map(|k| k.as_ptr().cast_mut())
            .collect::<Vec<_>>();
        let mut map = Box::new(sys::mpv_node_list {
            num: nodes.len() as i32,
            values: nodes.as_mut_ptr(),
            keys: key_ptrs.as_mut_ptr(),
        });
        let root = list_node(sys::mpv_format::MPV_FORMAT_NODE_MAP, map.as_mut());
        Ok(Self {
            _keys: keys,
            _values: values,
            _nodes: nodes,
            _key_ptrs: key_ptrs,
            _map: map,
            root,
        })
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut sys::mpv_node {
        &mut self.root
    }
}

// SAFETY: see [`LoadFileCommand`]; every pointer refers to storage owned here.
unsafe impl Send for NamedArgsCommand {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_arguments_use_named_arguments_including_resolution_and_z() {
        let args = overlay_arguments(100, Some("{\\pos(1,2)}hi"), 1920, 1080, 1);
        let pairs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(
            pairs,
            vec![
                ("id", "100"),
                ("format", "ass-events"),
                ("data", "{\\pos(1,2)}hi"),
                ("res_x", "1920"),
                ("res_y", "1080"),
                ("z", "1"),
            ]
        );
    }

    #[test]
    fn removing_an_overlay_sends_the_required_data_field_empty() {
        let args = overlay_arguments(101, None, 1920, 1080, 0);
        let pairs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(pairs, vec![("id", "101"), ("format", "none"), ("data", "")]);
    }

    #[test]
    fn named_args_command_is_a_map_rooted_at_the_command_name() {
        let args = overlay_arguments(7, Some("x"), 1280, 720, 0);
        let mut command = NamedArgsCommand::new("osd-overlay", &args).expect("command");
        let root = unsafe { &*command.as_mut_ptr() };
        assert_eq!(
            root.format,
            sys::mpv_format::MPV_FORMAT_NODE_MAP,
            "named arguments must be the command node itself, not an array entry"
        );
        let map = unsafe { &*root.u.list };
        assert_eq!(
            map.num as usize,
            args.len() + 1,
            "_name plus every argument"
        );
        assert!(!map.keys.is_null(), "the argument map must carry keys");

        let entry = |index: usize| -> (String, sys::mpv_node) {
            let key = unsafe { *map.keys.add(index) };
            let key = unsafe { std::ffi::CStr::from_ptr(key) }
                .to_string_lossy()
                .into_owned();
            let value = unsafe { *map.values.add(index) };
            (key, value)
        };

        let (name_key, name_value) = entry(0);
        assert_eq!(name_key, "_name");
        assert_eq!(name_value.format, sys::mpv_format::MPV_FORMAT_STRING);
        let name = unsafe { std::ffi::CStr::from_ptr(name_value.u.string) }
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, "osd-overlay");

        for (index, (key, value)) in args.iter().enumerate() {
            let (entry_key, entry_value) = entry(index + 1);
            assert_eq!(&entry_key, key);
            assert_eq!(entry_value.format, sys::mpv_format::MPV_FORMAT_STRING);
            let text = unsafe { std::ffi::CStr::from_ptr(entry_value.u.string) }
                .to_string_lossy()
                .into_owned();
            assert_eq!(&text, value);
        }
    }
}
