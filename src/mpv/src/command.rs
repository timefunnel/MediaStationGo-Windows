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
