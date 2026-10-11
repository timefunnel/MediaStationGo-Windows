//! Optional video component operations. Native runtime changes apply on startup.

use jfn_frame_interpolation::{component, components::ComponentKind};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::LazyLock;

type Failure = (&'static str, String);

#[derive(Default)]
struct OperationState {
    busy: bool,
    restarting: bool,
    progress: Option<Value>,
    error: Option<Failure>,
}

static STATE: LazyLock<Mutex<OperationState>> =
    LazyLock::new(|| Mutex::new(OperationState::default()));

static EXTRA_STATES: LazyLock<[Mutex<OperationState>; 3]> =
    LazyLock::new(|| std::array::from_fn(|_| Mutex::new(OperationState::default())));
static RESTARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn state_for(kind: ComponentKind) -> &'static Mutex<OperationState> {
    match kind {
        ComponentKind::Rife => &STATE,
        ComponentKind::RtxVideo => &EXTRA_STATES[0],
        ComponentKind::DlssNr => &EXTRA_STATES[1],
        ComponentKind::NvidiaVfg => &EXTRA_STATES[2],
    }
}
fn cache_root_for(kind: ComponentKind) -> PathBuf {
    jfn_paths::cache_dir().join(kind.cache_name())
}

pub(crate) fn status_for(kind: ComponentKind) -> Value {
    let state = state_for(kind).lock();
    match component::status(&cache_root_for(kind)) {
        Ok(mut status) => {
            status["busy"] = json!(state.busy);
            status["restarting"] =
                json!(state.restarting || RESTARTING.load(std::sync::atomic::Ordering::Acquire));
            status["progress"] = json!(state.progress);
            status["error"] = state.error.as_ref().map_or(
                Value::Null,
                |(code, message)| json!({"code": code, "message": message}),
            );
            status
        }
        Err(error) => {
            json!({"busy": state.busy, "error": {"code": error.code, "message": error.detail}})
        }
    }
}

fn available(state: &OperationState) -> Result<(), Failure> {
    if !component::SUPPORTED {
        return Err((
            "frame_interpolation_platform_unsupported",
            "插帧组件只支持 Windows x64".into(),
        ));
    }
    if state.busy {
        return Err((
            "frame_interpolation_component_busy",
            "组件正在下载或校验，请稍候".into(),
        ));
    }
    if state.restarting || RESTARTING.load(std::sync::atomic::Ordering::Acquire) {
        return Err((
            "frame_interpolation_component_busy",
            "播放器正在退出并重新启动，请勿重复操作".into(),
        ));
    }
    Ok(())
}

fn prepare(state: &mut OperationState, install: bool, kind: ComponentKind) -> Result<(), Failure> {
    available(state)?;
    let status =
        component::status(&cache_root_for(kind)).map_err(|error| (error.code, error.detail))?;
    if status["restartRequired"] == true {
        return Err((
            "frame_interpolation_component_busy",
            "组件变更已排队，请退出所有播放器实例后重新启动".into(),
        ));
    }
    if (status["installed"] == true) == install {
        return Err((
            "frame_interpolation_component_invalid",
            if install {
                "组件已安装，请先卸载"
            } else {
                "尚未安装组件"
            }
            .into(),
        ));
    }
    state.error = None;
    state.progress = None;
    Ok(())
}

pub(crate) fn install_for(kind: ComponentKind) -> Result<(), Failure> {
    {
        let mut state = state_for(kind).lock();
        prepare(&mut state, true, kind)?;
        state.busy = true;
        state.progress = Some(json!({"phase": "checking"}));
    }
    let spawned = std::thread::Builder::new()
        .name("rife-component-install".into())
        .spawn(move || {
            let result = install_package(kind);
            let mut state = state_for(kind).lock();
            state.busy = false;
            state.progress = None;
            state.error = result.err();
        });
    if let Err(error) = spawned {
        let failure = (
            "frame_interpolation_component_install_failed",
            format!("无法启动组件安装任务：{error}"),
        );
        let mut state = state_for(kind).lock();
        state.busy = false;
        state.progress = None;
        state.error = Some(failure.clone());
        return Err(failure);
    }
    Ok(())
}

fn install_package(kind: ComponentKind) -> Result<(), Failure> {
    let _lease = component::download_lease(&cache_root_for(kind))
        .map_err(|error| (error.code, error.detail))?;
    let directory = component::download_directory(&cache_root_for(kind))
        .map_err(|error| (error.code, error.detail))?;
    let (archive, sha) = crate::business_update::download_component(
        kind,
        &directory,
        |downloaded, total, source| {
            state_for(kind).lock().progress = Some(
                json!({"phase": "downloading", "downloadedBytes": downloaded, "totalBytes": total, "source": source}),
            );
        },
    )?;
    state_for(kind).lock().progress = Some(json!({"phase": "verifying"}));
    component::queue_install(&cache_root_for(kind), &archive, &sha)
        .map_err(|error| (error.code, error.detail))
}

pub(crate) fn uninstall_for(kind: ComponentKind) -> Result<(), Failure> {
    let mut state = state_for(kind).lock();
    prepare(&mut state, false, kind)?;
    component::queue_uninstall(&cache_root_for(kind)).map_err(|error| (error.code, error.detail))
}

pub(crate) fn cancel_install_for(kind: ComponentKind) -> Result<(), Failure> {
    let mut state = state_for(kind).lock();
    available(&state)?;
    component::cancel_pending_install(&cache_root_for(kind))
        .map_err(|error| (error.code, error.detail))?;
    state.error = None;
    state.progress = None;
    Ok(())
}

pub(crate) fn restart_for(kind: ComponentKind) -> Result<(), Failure> {
    available(&state_for(kind).lock())?;
    RESTARTING
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .map_err(|_| {
            (
                "frame_interpolation_component_busy",
                "已有重启请求正在处理".into(),
            )
        })?;
    let result = (|| {
        // Reserve restart before inspecting any state, so a second component
        // cannot start extracting while the helper waits for this process.
        for component in ComponentKind::ALL {
            if state_for(component).lock().busy {
                return Err((
                    "frame_interpolation_component_busy",
                    "其他组件仍在下载、选择或校验，请完成后再重启".into(),
                ));
            }
        }
        let mut state = state_for(kind).lock();
        let status =
            component::status(&cache_root_for(kind)).map_err(|error| (error.code, error.detail))?;
        require_pending_install(&status)?;
        launch_restart_helper().map_err(|detail| ("frame_interpolation_restart_failed", detail))?;
        state.restarting = true;
        Ok(())
    })();
    if result.is_err() {
        RESTARTING.store(false, std::sync::atomic::Ordering::Release);
    }
    result
}

fn require_pending_install(status: &Value) -> Result<(), Failure> {
    if status["restartRequired"] != true || status["pendingOperation"] != "install" {
        return Err((
            "frame_interpolation_component_invalid",
            "没有待重启生效的组件安装操作".into(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn launch_restart_helper() -> Result<(), String> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    let executable =
        std::env::current_exe().map_err(|error| format!("无法确认当前播放器路径：{error}"))?;
    let directory = executable.parent().ok_or("当前播放器没有有效目录")?;
    let helper = directory.join("mediastation-portable-updater.exe");
    let working_directory =
        std::env::current_dir().map_err(|error| format!("无法确认当前工作目录：{error}"))?;
    let mut child = Command::new(helper)
        .arg("--restart-only")
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .arg("--working-dir")
        .arg(working_directory)
        .arg("--")
        .args(std::env::args_os().skip(1))
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动重启助手，播放器不会退出：{error}"))?;

    // Do not shut down until the helper has opened this process for waiting.
    // A short readiness handshake also exposes missing/older helpers safely.
    let readiness = (|| {
        let stdout = child.stdout.take().ok_or("无法读取重启助手应答")?;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("rife-restart-readiness".into())
            .spawn(move || {
                let mut line = String::new();
                let result = BufReader::new(stdout)
                    .read_line(&mut line)
                    .map(|_| line)
                    .map_err(|error| format!("无法读取重启助手应答：{error}"));
                if sender.send(result).is_err() {
                    jfn_logging::log(
                        jfn_logging::CATEGORY_CEF,
                        jfn_logging::LEVEL_INFO,
                        "Restart readiness receiver closed after cancellation",
                    );
                }
            })
            .map_err(|error| format!("无法跟踪重启助手：{error}"))?;
        let ready = receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| format!("重启助手未确认就绪：{error}"))??;
        if ready.trim() != "MEDIASTATION_RESTART_READY" {
            return Err("重启助手应答无效，播放器不会退出".into());
        }
        Ok(())
    })();
    if let Err(error) = readiness {
        child
            .kill()
            .map_err(|cause| format!("{error}；无法停止本次重启助手：{cause}"))?;
        child
            .wait()
            .map_err(|cause| format!("{error}；无法回收本次重启助手：{cause}"))?;
        return Err(error);
    }
    Ok(())
}

#[cfg(not(windows))]
fn launch_restart_helper() -> Result<(), String> {
    Err("自动重启只支持 Windows".into())
}

pub(crate) fn status() -> Value {
    status_for(ComponentKind::Rife)
}
pub(crate) fn install() -> Result<(), Failure> {
    install_for(ComponentKind::Rife)
}
pub(crate) fn uninstall() -> Result<(), Failure> {
    uninstall_for(ComponentKind::Rife)
}
pub(crate) fn cancel_install() -> Result<(), Failure> {
    cancel_install_for(ComponentKind::Rife)
}
pub(crate) fn restart() -> Result<(), Failure> {
    restart_for(ComponentKind::Rife)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_restart_requires_a_pending_install_not_uninstall() {
        assert!(
            require_pending_install(
                &json!({"restartRequired": true, "pendingOperation": "install"})
            )
            .is_ok()
        );
        for status in [
            json!({}),
            json!({"restartRequired": false, "pendingOperation": "install"}),
            json!({"restartRequired": true, "pendingOperation": "uninstall"}),
        ] {
            assert_eq!(
                require_pending_install(&status).unwrap_err().0,
                "frame_interpolation_component_invalid"
            );
        }
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn component_operations_are_blocked_during_download_or_restart() {
        for state in [
            OperationState {
                busy: true,
                ..OperationState::default()
            },
            OperationState {
                restarting: true,
                ..OperationState::default()
            },
        ] {
            assert_eq!(
                available(&state).unwrap_err().0,
                "frame_interpolation_component_busy"
            );
        }
    }
}

pub(crate) fn import_for(kind: ComponentKind) -> Result<(), Failure> {
    {
        let mut state = state_for(kind).lock();
        prepare(&mut state, true, kind)?;
        state.busy = true;
        state.progress = Some(json!({"phase": "selecting"}));
    }
    let spawned = std::thread::Builder::new()
        .name("video-component-import".into())
        .spawn(move || {
            let result = (|| {
                let Some(archive) = select_package()? else {
                    return Ok(());
                };
                state_for(kind).lock().progress = Some(json!({"phase": "verifying"}));
                let sha =
                    component::sha256(&archive).map_err(|error| (error.code, error.detail))?;
                component::queue_install(&cache_root_for(kind), &archive, &sha)
                    .map_err(|error| (error.code, error.detail))
            })();
            let mut state = state_for(kind).lock();
            state.busy = false;
            state.progress = None;
            state.error = result.err();
        });
    if let Err(error) = spawned {
        let mut state = state_for(kind).lock();
        state.busy = false;
        state.progress = None;
        let failure = ("component_import_failed", error.to_string());
        state.error = Some(failure.clone());
        return Err(failure);
    }
    Ok(())
}

#[cfg(windows)]
fn select_package() -> Result<Option<PathBuf>, Failure> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, OFN_DONTADDTORECENT, OFN_FILEMUSTEXIST,
        OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let mut filename = vec![0_u16; 32768];
    let filter: Vec<u16> = "组件安装包 (*.zip)\0*.zip\0\0".encode_utf16().collect();
    let title: Vec<u16> = "选择 MediaStationGo 组件包\0".encode_utf16().collect();
    let mut options: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    options.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    options.lpstrFile = filename.as_mut_ptr();
    options.nMaxFile = filename.len() as u32;
    options.lpstrFilter = filter.as_ptr();
    options.lpstrTitle = title.as_ptr();
    options.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_DONTADDTORECENT;
    if unsafe { GetOpenFileNameW(&mut options) } == 0 {
        let code = unsafe { CommDlgExtendedError() };
        return if code == 0 {
            Ok(None)
        } else {
            Err((
                "component_dialog_failed",
                format!("无法选择组件包：0x{code:08x}"),
            ))
        };
    }
    use std::os::windows::ffi::OsStringExt as _;
    let length = filename
        .iter()
        .position(|value| *value == 0)
        .ok_or(("component_dialog_failed", "组件包路径无效".into()))?;
    Ok(Some(PathBuf::from(std::ffi::OsString::from_wide(
        &filename[..length],
    ))))
}
#[cfg(not(windows))]
fn select_package() -> Result<Option<PathBuf>, Failure> {
    Err((
        "component_platform_unsupported",
        "组件只支持 Windows x64".into(),
    ))
}
