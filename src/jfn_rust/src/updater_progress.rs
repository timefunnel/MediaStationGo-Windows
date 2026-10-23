//! The portable updater owns this window after the main application exits.
//! File operations run on a worker so hashing, extraction and rollback cannot
//! block painting or the native progress animation.
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontW, DeleteObject, GetSysColorBrush, HFONT, SetBkMode, TRANSPARENT,
    UpdateWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    ICC_PROGRESS_CLASS, INITCOMMONCONTROLSEX, InitCommonControlsEx, PBM_SETMARQUEE, PBM_SETPOS,
    PBS_MARQUEE, PROGRESS_CLASSW,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    stage: &'static str,
    percent: Option<u32>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            stage: "正在准备更新…",
            percent: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct Progress(Arc<Mutex<Snapshot>>);

impl Progress {
    pub fn stage(&self, stage: &'static str) {
        *self.0.lock() = Snapshot {
            stage,
            percent: None,
        };
    }

    pub fn fraction(&self, stage: &'static str, completed: u64, total: u64) {
        *self.0.lock() = Snapshot {
            stage,
            percent: (total > 0).then(|| (completed.saturating_mul(100) / total).min(100) as u32),
        };
    }
}

pub fn run(
    work: impl FnOnce(Progress) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    let mut window = ProgressWindow::new()?;
    let progress = Progress::default();
    window.refresh(progress.0.lock().clone());
    let worker_progress = progress.clone();
    let worker = std::thread::Builder::new()
        .name("portable-update".into())
        .spawn(move || work(worker_progress))
        .map_err(|error| format!("cannot start update worker: {error}"))?;
    while !worker.is_finished() {
        window.refresh(progress.0.lock().clone());
        let mut message = MSG::default();
        unsafe {
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    worker
        .join()
        .map_err(|_| "update worker panicked".to_string())?
}

struct ProgressWindow {
    window: HWND,
    status: HWND,
    bar: HWND,
    font: HFONT,
    dpi: i32,
    last: Option<Snapshot>,
}

impl ProgressWindow {
    fn new() -> Result<Self, String> {
        let class = wide("MediaStationGoUpdateProgress");
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let controls = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_PROGRESS_CLASS,
        };
        unsafe {
            let dpi = GetDpiForSystem() as i32;
            let width = 520 * dpi / 96;
            let height = 220 * dpi / 96;
            if InitCommonControlsEx(&controls) == 0 {
                return Err(window_error("initialize update progress controls"));
            }
            let window_class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_WINDOW + 1) as _,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            if RegisterClassW(&window_class) == 0 {
                return Err(window_error("register update progress window"));
            }
            let window = CreateWindowExW(
                WS_EX_APPWINDOW,
                class.as_ptr(),
                wide("MediaStationGo · 应用更新").as_ptr(),
                WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
                (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
                width,
                height,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if window.is_null() {
                return Err(window_error("create update progress window"));
            }
            let mut ui = Self {
                window,
                status: std::ptr::null_mut(),
                bar: std::ptr::null_mut(),
                font: std::ptr::null_mut(),
                dpi,
                last: None,
            };
            ui.font = CreateFontW(
                -16 * dpi / 96,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                wide("Microsoft YaHei UI").as_ptr(),
            );
            if ui.font.is_null() {
                return Err(window_error("create update progress font"));
            }
            // Applying files cannot safely be cancelled halfway through.
            EnableMenuItem(GetSystemMenu(window, 0), SC_CLOSE, MF_BYCOMMAND | MF_GRAYED);
            ui.status = ui.control(
                wide("STATIC").as_ptr(),
                "正在准备更新…",
                [26, 28, 460, 26],
                0,
            )?;
            ui.bar = ui.control(PROGRESS_CLASSW, "", [26, 70, 460, 18], PBS_MARQUEE)?;
            if SendMessageW(ui.bar, PBM_SETMARQUEE, 1, 30) == 0 {
                return Err("native update progress animation is unavailable".into());
            }
            ui.control(
                wide("STATIC").as_ptr(),
                "完成后会自动重新启动，无需重复操作。",
                [26, 112, 460, 30],
                0,
            )?;
            ShowWindow(window, SW_SHOWNORMAL);
            UpdateWindow(window);
            Ok(ui)
        }
    }

    fn control(
        &self,
        class: *const u16,
        text: &str,
        bounds: [i32; 4],
        style: u32,
    ) -> Result<HWND, String> {
        unsafe {
            let [x, y, width, height] = bounds.map(|value| value * self.dpi / 96);
            let control = CreateWindowExW(
                0,
                class,
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                width,
                height,
                self.window,
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            if control.is_null() {
                return Err(window_error("create update progress control"));
            }
            SendMessageW(control, WM_SETFONT, self.font as usize, 1);
            Ok(control)
        }
    }

    fn refresh(&mut self, snapshot: Snapshot) {
        if self.last.as_ref() == Some(&snapshot) {
            return;
        }
        let text = match snapshot.percent {
            Some(percent) => format!("{}  {percent}%", snapshot.stage),
            None => snapshot.stage.to_string(),
        };
        unsafe {
            SetWindowTextW(self.status, wide(&text).as_ptr());
            let style = GetWindowLongPtrW(self.bar, GWL_STYLE) as u32;
            let indeterminate = snapshot.percent.is_none();
            SetWindowLongPtrW(
                self.bar,
                GWL_STYLE,
                if indeterminate {
                    style | PBS_MARQUEE
                } else {
                    style & !PBS_MARQUEE
                } as isize,
            );
            SendMessageW(self.bar, PBM_SETMARQUEE, usize::from(indeterminate), 30);
            if let Some(percent) = snapshot.percent {
                SendMessageW(self.bar, PBM_SETPOS, percent as usize, 0);
            }
        }
        self.last = Some(snapshot);
    }
}

impl Drop for ProgressWindow {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE {
        return 0;
    }
    if message == WM_CTLCOLORSTATIC {
        unsafe {
            SetBkMode(wparam as _, TRANSPARENT as i32);
            return GetSysColorBrush(COLOR_WINDOW) as LRESULT;
        }
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn window_error(action: &str) -> String {
    format!("cannot {action}: {}", std::io::Error::last_os_error())
}
