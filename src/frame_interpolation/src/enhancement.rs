//! Playback plans for independently installed native video providers.
use super::{InterpolationError, component, components::ComponentKind};
use serde::{Deserialize, Serialize};
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
            json!({"runtimeVerified": runtime.installed, "playbackAvailable": runtime.installed && bridge_path().is_ok(),
            "experimental": false, "supportedDynamicRanges": ["sdr", "hdr10", "hlg"], "failureDetail": null, "defaults": Selection::default().payload()})
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
    if !path.is_file() || !path.with_file_name("nvngx.dll").is_file() {
        return Err(InterpolationError::new(
            "component_bridge_unavailable",
            "Native video bridge is missing from the application directory",
        ));
    }
    Ok(path)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Selection {
    pub vsr: bool,
    pub vfg: bool,
    pub nr: bool,
    pub vsr_quality: u32,
    pub vsr_target: u32,
    pub vfg_quality: u32,
    pub multiplier: u32,
    pub scene_detection: bool,
    pub nr_style: i32,
    pub nr_target: u32,
    pub nr_intensity: f32,
    pub nr_tone: f32,
    pub nr_structure: f32,
    pub nr_skin: f32,
    pub nr_auto_mask: bool,
    pub nr_ui_correction: bool,
}
impl Default for Selection {
    fn default() -> Self {
        Self {
            vsr: false,
            vfg: false,
            nr: false,
            vsr_quality: 4,
            vsr_target: 0,
            vfg_quality: 0,
            multiplier: 0,
            scene_detection: true,
            nr_style: 0,
            nr_target: 1080,
            nr_intensity: 1.,
            nr_tone: 1.,
            nr_structure: 1.,
            nr_skin: -1.,
            nr_auto_mask: false,
            nr_ui_correction: false,
        }
    }
}
impl Selection {
    pub fn parse(text: &str) -> Result<Self, InterpolationError> {
        let error = || {
            InterpolationError::new(
                "video_enhancement_request_invalid",
                "Invalid video enhancement parameters or values outside the supported range",
            )
        };
        if text.len() > 2048 || !text.trim_start().starts_with('{') {
            return Err(error());
        }
        let s: Self = serde_json::from_str(text).map_err(|_| error())?;
        if !(1..=4).contains(&s.vsr_quality)
            || !matches!(s.vsr_target, 0 | 1440 | 2160 | 2880 | 3456 | 4032 | 4320)
            || s.vfg_quality > 2
            || !(s.multiplier == 0 || (2..=3).contains(&s.multiplier))
            || !(0..=2).contains(&s.nr_style)
            || !matches!(s.nr_target, 0 | 480 | 720 | 900 | 1080 | 1440)
            || [s.nr_intensity, s.nr_tone, s.nr_structure]
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            || !s.nr_skin.is_finite()
            || !(s.nr_skin == -1.0 || (0.0..=2.0).contains(&s.nr_skin))
        {
            return Err(error());
        }
        Ok(s)
    }
    pub fn payload(self) -> Value {
        json!(self)
    }
    pub fn enabled(self) -> bool {
        self.vsr || self.vfg || self.nr
    }
    pub fn filter(
        self,
        rife_filter: Option<&str>,
        hdr: bool,
    ) -> Result<String, InterpolationError> {
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
        if self.enabled() {
            if !hdr {
                filters.push("@nvconvert:d3d11vpp=format=bgra".into());
            }
            // HDR remains P010 throughout. Generate frames after HDR restoration.
            let providers = if hdr {
                [
                    (self.nr, ComponentKind::DlssNr, 3, "enhance_nr"),
                    (self.vsr, ComponentKind::RtxVideo, 1, "enhance_vsr"),
                    (self.vfg, ComponentKind::NvidiaVfg, 2, "enhance_vfg"),
                ]
            } else {
                [
                    (self.nr, ComponentKind::DlssNr, 3, "enhance_nr"),
                    (self.vfg, ComponentKind::NvidiaVfg, 2, "enhance_vfg"),
                    (self.vsr, ComponentKind::RtxVideo, 1, "enhance_vsr"),
                ]
            };
            for (enabled, kind, mode, label) in providers {
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
                let parameters = match kind {
                    ComponentKind::RtxVideo => format!(
                        ":quality={}:target-height={}",
                        self.vsr_quality, self.vsr_target
                    ),
                    ComponentKind::NvidiaVfg => format!(
                        ":quality={}:multiplier={}:scene-detection={}",
                        self.vfg_quality,
                        self.multiplier,
                        u8::from(self.scene_detection)
                    ),
                    ComponentKind::DlssNr => format!(
                        ":nr-style={}:nr-target={}:nr-intensity={}:nr-tone={}:nr-structure={}:nr-skin={}:nr-auto-mask={}:nr-ui-correction={}",
                        self.nr_style,
                        self.nr_target,
                        self.nr_intensity,
                        self.nr_tone,
                        self.nr_structure,
                        self.nr_skin,
                        u8::from(self.nr_auto_mask),
                        u8::from(self.nr_ui_correction)
                    ),
                    ComponentKind::Rife => unreachable!(),
                };
                filters.push(format!(
                    "@{label}:nvvideo=mode={mode}:bridge={}:runtime={}:cache={}{parameters}",
                    quote(&bridge_path()?),
                    quote(&runtime.root),
                    quote(&runtime.cache)
                ));
            }
        }
        if self.enabled() && !hdr {
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
            "{\"vsrQuality\":0}",
            "{\"vsrTarget\":1080}",
            "{\"vfgQuality\":3}",
            "{\"multiplier\":4}",
            "{\"multiplier\":8}",
            "{\"multiplier\":9}",
            "{\"multiplier\":1}",
            "{\"nrStyle\":3}",
            "{\"nrIntensity\":1.1}",
            "{\"nrTone\":null}",
            "{\"nrSkin\":-0.5}",
            "{\"nrAutoMask\":1}",
            "{\"nrStructure\":1e300}",
        ] {
            assert!(
                Selection::parse(text).is_err(),
                "accepted invalid request: {text}"
            );
        }
        assert_eq!(Selection::parse("{}").unwrap(), Selection::default());
        for multiplier in 2..=3 {
            assert_eq!(
                Selection::parse(&format!(r#"{{"vfg":true,"multiplier":{multiplier}}}"#))
                    .unwrap()
                    .multiplier,
                multiplier
            );
        }
        assert_eq!(
            Selection::parse(r#"{"vfg":true,"multiplier":0}"#)
                .unwrap()
                .multiplier,
            0
        );
        assert_eq!(
            Selection::parse("{\"vsr\":true,\"vfg\":false}").unwrap(),
            Selection {
                vsr: true,
                vfg: false,
                ..Selection::default()
            }
        );
        assert_eq!(
            Selection {
                vsr: false,
                vfg: true,
                ..Selection::default()
            }
            .filter(Some("rife"), false)
            .unwrap_err()
            .code,
            "video_enhancement_conflict"
        );
        assert_eq!(
            Selection::default().filter(Some("rife"), false).unwrap(),
            "rife"
        );
    }
    #[test]
    fn all_provider_parameters_round_trip_without_losing_values() {
        let selection = Selection::parse(r#"{"nr":true,"vsr":true,"vfg":true,"vsrQuality":2,"vsrTarget":2160,"vfgQuality":2,"multiplier":3,"sceneDetection":false,"nrStyle":2,"nrIntensity":0.7,"nrTone":0.6,"nrStructure":0.8,"nrSkin":2,"nrAutoMask":true,"nrUiCorrection":true}"#).unwrap();
        assert!(selection.enabled());
        assert_eq!(
            Selection::parse(&selection.payload().to_string()).unwrap(),
            selection
        );
        assert_eq!(selection.multiplier, 3);
        assert_eq!(selection.nr_style, 2);
    }
}
