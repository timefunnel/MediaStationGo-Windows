//! Optional RIFE component operations. Native runtime changes apply on startup.

use jfn_frame_interpolation::component;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::LazyLock;

type Failure = (&'static str, String);

#[derive(Default)]
struct OperationState {
    busy: bool,
    progress: Option<Value>,
    error: Option<Failure>,
}

static STATE: LazyLock<Mutex<OperationState>> =
    LazyLock::new(|| Mutex::new(OperationState::default()));

fn cache_root() -> PathBuf {
    jfn_paths::cache_dir().join("frame-interpolation")
}

pub(crate) fn status() -> Value {
    let state = STATE.lock();
    match component::status(&cache_root()) {
        Ok(mut status) => {
            status["busy"] = json!(state.busy);
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

fn prepare(state: &mut OperationState, install: bool) -> Result<(), Failure> {
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
    let status = component::status(&cache_root()).map_err(|error| (error.code, error.detail))?;
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

pub(crate) fn install() -> Result<(), Failure> {
    {
        let mut state = STATE.lock();
        prepare(&mut state, true)?;
        state.busy = true;
        state.progress = Some(json!({"phase": "checking"}));
    }
    let spawned = std::thread::Builder::new()
        .name("rife-component-install".into())
        .spawn(|| {
            let result = install_package();
            let mut state = STATE.lock();
            state.busy = false;
            state.progress = None;
            state.error = result.err();
        });
    if let Err(error) = spawned {
        let failure = (
            "frame_interpolation_component_install_failed",
            format!("无法启动组件安装任务：{error}"),
        );
        let mut state = STATE.lock();
        state.busy = false;
        state.progress = None;
        state.error = Some(failure.clone());
        return Err(failure);
    }
    Ok(())
}

fn install_package() -> Result<(), Failure> {
    let _lease =
        component::download_lease(&cache_root()).map_err(|error| (error.code, error.detail))?;
    let directory =
        component::download_directory(&cache_root()).map_err(|error| (error.code, error.detail))?;
    let (archive, sha) = crate::business_update::download_rife_component(
        &directory,
        |downloaded, total, source| {
            STATE.lock().progress = Some(
                json!({"phase": "downloading", "downloadedBytes": downloaded, "totalBytes": total, "source": source}),
            );
        },
    )?;
    STATE.lock().progress = Some(json!({"phase": "verifying"}));
    component::queue_install(&cache_root(), &archive, &sha)
        .map_err(|error| (error.code, error.detail))
}

pub(crate) fn uninstall() -> Result<(), Failure> {
    let mut state = STATE.lock();
    prepare(&mut state, false)?;
    component::queue_uninstall(&cache_root()).map_err(|error| (error.code, error.detail))
}
