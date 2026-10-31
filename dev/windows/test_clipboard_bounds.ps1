param()
$ErrorActionPreference = 'Stop'

# Exercise the production reader with a fixture clipboard and the real Windows
# UTF-16 conversion. Reject writes beyond the actual Rust allocation before the
# Win32 call can corrupt memory. The user's clipboard is never read or changed.
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$source = [IO.File]::ReadAllText((Join-Path $repoRoot 'src/windows/src/lib.rs'))
$start = $source.IndexOf('pub fn win_clipboard_read_text_async(')
$end = $source.IndexOf('/// Open an external URL', $start)
if ($start -lt 0 -or $end -lt 0) { throw 'Production clipboard reader was not found' }
$reader = $source.Substring($start, $end - $start)
$harness = @'
#![allow(non_snake_case)]
use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

struct TrackedAllocator;
static ALLOCATION: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
static ALLOCATION_SIZE: AtomicUsize = AtomicUsize::new(0);
static CLIPBOARD: AtomicPtr<u16> = AtomicPtr::new(std::ptr::null_mut());
static CONVERSIONS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for TrackedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        ALLOCATION_SIZE.store(layout.size(), Ordering::SeqCst);
        ALLOCATION.store(pointer, Ordering::SeqCst);
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}
#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;

const CF_UNICODETEXT: u32 = 13;
const CP_UTF8: u32 = 65001;
unsafe fn OpenClipboard(_: *mut c_void) -> i32 { 1 }
unsafe fn CloseClipboard() -> i32 { 1 }
unsafe fn GetClipboardData(format: u32) -> *mut c_void {
    assert_eq!(format, CF_UNICODETEXT);
    CLIPBOARD.load(Ordering::SeqCst).cast()
}
unsafe fn GlobalLock(handle: *mut c_void) -> *mut c_void { handle }
unsafe fn GlobalUnlock(_: *mut c_void) -> i32 { 1 }

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "WideCharToMultiByte"]
    fn native_conversion(cp: u32, flags: u32, wide: *const u16, wide_len: c_int,
        out: *mut u8, out_len: c_int, default: *const u8, used: *mut i32) -> c_int;
}
unsafe fn WideCharToMultiByte(cp: u32, flags: u32, wide: *const u16, wide_len: c_int,
    out: *mut u8, out_len: c_int, default: *const u8, used: *mut i32) -> c_int {
    if !out.is_null() {
        assert_eq!(out, ALLOCATION.load(Ordering::SeqCst), "conversion buffer must be tracked");
        let allocated = ALLOCATION_SIZE.load(Ordering::SeqCst);
        assert!(out_len >= 0 && out_len as usize <= allocated,
            "clipboard overflow: Win32 may write {out_len} bytes into a {allocated}-byte allocation");
        CONVERSIONS.fetch_add(1, Ordering::SeqCst);
    }
    unsafe { native_conversion(cp, flags, wide, wide_len, out, out_len, default, used) }
}

#[macro_export]
macro_rules! unexpected_diagnostic {
    ($($argument:tt)*) => { fail_diagnostic(format_args!($($argument)*)) };
}
fn fail_diagnostic(message: std::fmt::Arguments<'_>) { panic!("{message}"); }
mod tracing {
    pub(crate) use crate::unexpected_diagnostic as error;
}

__PRODUCTION_READER__

fn main() {
    let mut values = vec![String::new(), "https://new.example.test:8920/emby".into(),
        "中文账号🚀".into(), "quote'\"\\\r\nnext line".into(), "😀".repeat(64)];
    // Include allocator size boundaries where a trailing NUL used to overrun.
    for length in 1..=512 { values.push("x".repeat(length)); }
    let count = values.len();
    for expected in values {
        let mut wide: Vec<u16> = expected.encode_utf16().chain(std::iter::once(0)).collect();
        CLIPBOARD.store(wide.as_mut_ptr(), Ordering::SeqCst);
        win_clipboard_read_text_async(Box::new(move |actual| {
            assert_eq!(actual, expected, "pasted text must be preserved without the terminator");
        }));
    }
    println!("CLIPBOARD_BOUNDS_OK cases={count} checked_conversions={}", CONVERSIONS.load(Ordering::SeqCst));
}
'@
$buildRoot = Join-Path $repoRoot 'build'
$tempRoot = Join-Path $buildRoot ('clipboard-bounds-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null
try {
    $harnessPath = Join-Path $tempRoot 'clipboard_bounds.rs'
    $executable = Join-Path $tempRoot 'clipboard_bounds.exe'
    [IO.File]::WriteAllText($harnessPath, $harness.Replace('__PRODUCTION_READER__', $reader), [Text.UTF8Encoding]::new($false))
    & rustc --edition=2024 $harnessPath -o $executable
    if ($LASTEXITCODE -ne 0) { throw 'Clipboard regression harness compilation failed' }
    & $executable
    if ($LASTEXITCODE -ne 0) { throw 'Clipboard regression failed' }
} finally {
    $resolved = [IO.Path]::GetFullPath($tempRoot)
    if ([IO.Path]::GetDirectoryName($resolved) -ne [IO.Path]::GetFullPath($buildRoot)) { throw 'Unexpected test cleanup path' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
