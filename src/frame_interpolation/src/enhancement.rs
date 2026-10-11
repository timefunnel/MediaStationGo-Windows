//! Playback plans for independently installed native video providers.
use super::{InterpolationError, component, components::ComponentKind};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

struct Runtime {
    root: PathBuf,
    cache: PathBuf,
    _lease: File,
    installed: bool,
}
static RUNTIMES: OnceLock<BTreeMap<ComponentKind, Result<Runtime, InterpolationError>>> =
    OnceLock::new();

pub fn initialize(cache_directory: &Path) {
    RUNTIMES.get_or_init(|| {
        ComponentKind::ALL
            .into_iter()
            .filter(|kind| *kind != ComponentKind::Rife)
            .map(|kind| {
                let cache_root = cache_directory.join(kind.cache_name());
                let runtime = component::startup(&cache_root).and_then(|(root, lease)| {
                    let installed = root.is_dir();
                    if installed {
                        component::validate_component_directory(kind, &root)?;
                    }
                    let cache = cache_root.join("engine-cache");
                    // This directory belongs to the same component cleanup transaction.
                    component::prepare_runtime_cache(&cache_root)?;
                    Ok(Runtime {
                        root,
                        cache,
                        _lease: lease,
                        installed,
                    })
                });
                (kind, runtime)
            })
            .collect()
    });
}

pub fn status(kind: ComponentKind) -> Value {
    match runtime(kind) {
        Ok(runtime) => {
            json!({"runtimeVerified": runtime.installed, "playbackAvailable": runtime.installed && kind != ComponentKind::DlssNr && bridge_path().is_ok(),
            "experimental": kind == ComponentKind::DlssNr, "failureDetail": if kind == ComponentKind::DlssNr { Some("DLSSNR 推理接入尚未通过验证；当前仅支持运行库管理，暂不可用于播放") } else { None }})
        }
        Err(error) => {
            json!({"runtimeVerified": false, "playbackAvailable": false, "failureCode": error.code, "failureDetail": error.detail})
        }
    }
}
fn runtime(kind: ComponentKind) -> Result<&'static Runtime, InterpolationError> {
    RUNTIMES
        .get()
        .and_then(|runtimes| runtimes.get(&kind))
        .ok_or_else(|| {
            InterpolationError::new(
                "component_not_initialized",
                "Native component startup has not run",
            )
        })?
        .as_ref()
        .map_err(Clone::clone)
}
fn bridge_path() -> Result<PathBuf, InterpolationError> {
    let executable = std::env::current_exe().map_err(|cause| {
        InterpolationError::new("component_bridge_unavailable", cause.to_string())
    })?;
    let path = executable
        .parent()
        .ok_or_else(|| {
            InterpolationError::new("component_bridge_unavailable", "Executable has no parent")
        })?
        .join("video_enhancement.dll");
    if !path.is_file() {
        return Err(InterpolationError::new(
            "component_bridge_unavailable",
            "Native video bridge is missing from the application directory",
        ));
    }
    Ok(path)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    pub vsr: bool,
    pub vfg: bool,
}
impl Selection {
    pub fn parse(text: &str) -> Result<Self, InterpolationError> {
        let error = || {
            InterpolationError::new(
                "video_enhancement_request_invalid",
                "Expected only boolean vsr and vfg fields",
            )
        };
        if text.len() > 256 {
            return Err(error());
        }
        let value: Value = serde_json::from_str(text).map_err(|_| error())?;
        let object = value.as_object().ok_or_else(error)?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "vsr" | "vfg"))
        {
            return Err(error());
        }
        let boolean = |key| -> Result<bool, InterpolationError> {
            match object.get(key) {
                None => Ok(false),
                Some(value) => value.as_bool().ok_or_else(error),
            }
        };
        Ok(Self {
            vsr: boolean("vsr")?,
            vfg: boolean("vfg")?,
        })
    }
    pub fn payload(self) -> Value {
        json!({"vsr": self.vsr, "vfg": self.vfg})
    }
    pub fn filter(self, rife_filter: Option<&str>) -> Result<String, InterpolationError> {
        if self.vfg && rife_filter.is_some() {
            return Err(InterpolationError::new(
                "video_enhancement_conflict",
                "RIFE and NVIDIA VFG cannot run together",
            ));
        }
        let mut filters = rife_filter
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if self.vsr || self.vfg {
            filters.push("@nvconvert:d3d11vpp=format=bgra".into());
            for (enabled, kind, mode, label) in [
                (self.vfg, ComponentKind::NvidiaVfg, 2, "enhance_vfg"),
                (self.vsr, ComponentKind::RtxVideo, 1, "enhance_vsr"),
            ] {
                if !enabled {
                    continue;
                }
                let runtime = runtime(kind)?;
                if !runtime.installed {
                    return Err(InterpolationError::new(
                        "video_enhancement_not_installed",
                        format!("{} is not installed", kind.name()),
                    ));
                }
                let quote = |path: &Path| {
                    let value = path.to_string_lossy();
                    format!("%{}%{}", value.len(), value)
                };
                filters.push(format!(
                    "@{label}:nvvideo=mode={mode}:bridge={}:runtime={}:cache={}",
                    quote(&bridge_path()?),
                    quote(&runtime.root),
                    quote(&runtime.cache)
                ));
            }
        }
        if self.vsr || self.vfg {
            filters.push("@nvoutput:d3d11vpp=format=nv12".into());
        }
        Ok(filters.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_selection_rejects_unknown_capabilities_and_conflicting_interpolators() {
        for text in [
            "null",
            "[]",
            "{\"vsr\":1}",
            "{\"vfg\":\"true\"}",
            "{\"dlssnr\":true}",
        ] {
            assert!(Selection::parse(text).is_err());
        }
        assert_eq!(Selection::parse("{}").unwrap(), Selection::default());
        assert_eq!(
            Selection::parse("{\"vsr\":true,\"vfg\":false}").unwrap(),
            Selection {
                vsr: true,
                vfg: false
            }
        );
        assert_eq!(
            Selection {
                vsr: false,
                vfg: true
            }
            .filter(Some("rife"))
            .unwrap_err()
            .code,
            "video_enhancement_conflict"
        );
        assert_eq!(Selection::default().filter(Some("rife")).unwrap(), "rife");
    }
}
