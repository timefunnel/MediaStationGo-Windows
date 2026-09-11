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
/// passing `data = None` removes the overlay again (`format=none`).
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
        ],
    }
}

/// Owned command tree for any `command <name> <map>` call made through
/// `mpv_command_node_async`. Same lifetime rules as [`LoadFileCommand`].
pub(crate) struct NamedArgsCommand {
    // Kept alive so the pointers inside `root` stay valid; same convention as
    // `LoadFileCommand` above.
    _strings: Vec<CString>,
    _keys: Vec<CString>,
    _values: Vec<CString>,
    _value_nodes: Vec<sys::mpv_node>,
    _key_ptrs: Vec<*mut c_char>,
    _map: Box<sys::mpv_node_list>,
    _arguments: Vec<sys::mpv_node>,
    _argument_list: Box<sys::mpv_node_list>,
    root: sys::mpv_node,
}

impl NamedArgsCommand {
    pub(crate) fn new(name: &str, args: &[(String, String)]) -> Result<Self, NulError> {
        let strings = vec![CString::new(name)?];
        let keys = args
            .iter()
            .map(|(key, _)| CString::new(key.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        let values = args
            .iter()
            .map(|(_, value)| CString::new(value.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut value_nodes: Vec<sys::mpv_node> = values.iter().map(|v| string_node(v)).collect();
        let mut key_ptrs = keys
            .iter()
            .map(|k| k.as_ptr().cast_mut())
            .collect::<Vec<_>>();
        let mut map = Box::new(sys::mpv_node_list {
            num: value_nodes.len() as i32,
            values: value_nodes.as_mut_ptr(),
            keys: key_ptrs.as_mut_ptr(),
        });
        let mut arguments = vec![
            string_node(&strings[0]),
            list_node(sys::mpv_format::MPV_FORMAT_NODE_MAP, map.as_mut()),
        ];
        let mut argument_list = Box::new(sys::mpv_node_list {
            num: arguments.len() as i32,
            values: arguments.as_mut_ptr(),
            keys: std::ptr::null_mut(),
        });
        let root = list_node(
            sys::mpv_format::MPV_FORMAT_NODE_ARRAY,
            argument_list.as_mut(),
        );
        Ok(Self {
            _strings: strings,
            _keys: keys,
            _values: values,
            _value_nodes: value_nodes,
            _key_ptrs: key_ptrs,
            _map: map,
            _arguments: arguments,
            _argument_list: argument_list,
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
    fn removing_an_overlay_only_sends_the_id() {
        let args = overlay_arguments(101, None, 1920, 1080, 0);
        let pairs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(pairs, vec![("id", "101"), ("format", "none")]);
    }

    #[test]
    fn named_args_command_builds_a_two_element_array_with_a_map() {
        let args = overlay_arguments(7, Some("x"), 1280, 720, 0);
        let mut command = NamedArgsCommand::new("osd-overlay", &args).expect("command");
        let root = unsafe { &*command.as_mut_ptr() };
        assert_eq!(root.format, sys::mpv_format::MPV_FORMAT_NODE_ARRAY);
        let list = unsafe { &*root.u.list };
        assert_eq!(list.num, 2);
        assert!(list.keys.is_null(), "the argument array must be positional");
        let map_node = unsafe { &*list.values.add(1) };
        assert_eq!(map_node.format, sys::mpv_format::MPV_FORMAT_NODE_MAP);
        let map = unsafe { &*map_node.u.list };
        assert_eq!(map.num as usize, args.len());
        assert!(!map.keys.is_null(), "the argument map must carry keys");
    }
}
