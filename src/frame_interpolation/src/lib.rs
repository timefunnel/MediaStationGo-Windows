use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const VAPOURSYNTH_VERSION: &str = "R65";
const VS_MLRT_VERSION: &str = "v15.16";
const TENSORRT_VERSION: &str = "TensorRT-RTX 1.4.0.76";
const MODEL_NAME: &str = "RIFE v4.25 Lite";
const MODEL_SHA256: &str = "026605086bd5782581cb3d846e16eec326d368af109f78cf8113883bff2e9e66";
const PLAYER_SCRIPT: &str = include_str!("rife_player.vpy");

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InterpolationMode {
    #[default]
    Off,
    Auto,
    Fps60,
    Fps90,
    Fps120,
}

impl InterpolationMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "auto" => Some(Self::Auto),
            "60" => Some(Self::Fps60),
            "90" => Some(Self::Fps90),
            "120" => Some(Self::Fps120),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Fps60 => "60",
            Self::Fps90 => "90",
            Self::Fps120 => "120",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterpolationError {
    pub code: &'static str,
    pub detail: String,
}

impl InterpolationError {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for InterpolationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for InterpolationError {}

#[derive(Clone, Debug, Default)]
pub struct CapabilityReport {
    pub ready: bool,
    pub gpu_name: Option<String>,
    pub gpu_uuid: Option<String>,
    pub driver_version: Option<String>,
    pub runtime_path: Option<PathBuf>,
    pub vapoursynth_version: Option<String>,
    pub vs_mlrt_version: Option<String>,
    pub tensorrt_version: Option<String>,
    pub model_name: Option<String>,
    pub redistributable: bool,
    pub failure: Option<InterpolationError>,
}

#[derive(Clone, Debug)]
struct GpuInfo {
    name: String,
    uuid: String,
    driver: String,
}

#[derive(Clone, Debug)]
struct RuntimeLayout {
    root: PathBuf,
    bin: PathBuf,
    python_lib: PathBuf,
    plugin_dir: PathBuf,
    plugin: PathBuf,
    scripts: PathBuf,
    model: PathBuf,
    engines: PathBuf,
    manifest: RuntimeManifest,
}

#[derive(Clone, Debug)]
struct RuntimeManifest {
    vapoursynth: String,
    vs_mlrt: String,
    tensorrt: String,
    model: String,
    model_sha256: String,
    redistributable: bool,
}

#[derive(Clone, Debug)]
struct RuntimeContext {
    report: CapabilityReport,
    layout: Option<RuntimeLayout>,
    gpu: Option<GpuInfo>,
    script_path: Option<PathBuf>,
    _vsscript_module: Option<usize>,
}

static CONTEXT: OnceLock<RuntimeContext> = OnceLock::new();

#[cfg(target_os = "windows")]
#[link(name = "ucrt")]
unsafe extern "C" {
    fn _wputenv_s(variable_name: *const u16, value: *const u16) -> i32;
}

pub fn initialize(cache_root: &Path) -> &'static CapabilityReport {
    &CONTEXT.get_or_init(|| build_context(cache_root)).report
}

pub fn capability_report() -> CapabilityReport {
    CONTEXT
        .get()
        .map(|context| context.report.clone())
        .unwrap_or_else(|| CapabilityReport {
            failure: Some(InterpolationError::new(
                "frame_interpolation_not_initialized",
                "The frame interpolation runtime has not been initialized",
            )),
            ..CapabilityReport::default()
        })
}

fn build_context(cache_root: &Path) -> RuntimeContext {
    match try_build_context(cache_root) {
        Ok(context) => context,
        Err(error) => RuntimeContext {
            report: CapabilityReport {
                failure: Some(error),
                ..CapabilityReport::default()
            },
            layout: None,
            gpu: None,
            script_path: None,
            _vsscript_module: None,
        },
    }
}

fn try_build_context(cache_root: &Path) -> Result<RuntimeContext, InterpolationError> {
    if !cfg!(target_os = "windows") {
        return Err(InterpolationError::new(
            "frame_interpolation_platform_unsupported",
            "RTX frame interpolation is supported only on Windows",
        ));
    }
    let gpu = probe_gpu()?;
    let layout = probe_runtime(cache_root)?;
    let script_path = install_player_script(cache_root)?;
    configure_process_environment(&layout)?;
    let vsscript_module = preload_vsscript(&layout)?;
    let report = CapabilityReport {
        ready: true,
        gpu_name: Some(gpu.name.clone()),
        gpu_uuid: Some(gpu.uuid.clone()),
        driver_version: Some(gpu.driver.clone()),
        runtime_path: Some(layout.root.clone()),
        vapoursynth_version: Some(layout.manifest.vapoursynth.clone()),
        vs_mlrt_version: Some(layout.manifest.vs_mlrt.clone()),
        tensorrt_version: Some(layout.manifest.tensorrt.clone()),
        model_name: Some(layout.manifest.model.clone()),
        redistributable: layout.manifest.redistributable,
        failure: None,
    };
    Ok(RuntimeContext {
        report,
        layout: Some(layout),
        gpu: Some(gpu),
        script_path: Some(script_path),
        _vsscript_module: Some(vsscript_module),
    })
}

fn probe_gpu() -> Result<GpuInfo, InterpolationError> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,uuid,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .map_err(|error| {
            InterpolationError::new(
                "frame_interpolation_nvidia_smi_unavailable",
                format!("nvidia-smi could not be started: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(InterpolationError::new(
            "frame_interpolation_nvidia_driver_unavailable",
            format!("nvidia-smi exited with status {}", output.status),
        ));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        InterpolationError::new(
            "frame_interpolation_nvidia_output_invalid",
            "nvidia-smi returned non-UTF-8 output",
        )
    })?;
    let line = stdout.lines().next().unwrap_or_default();
    let mut fields = line.split(',').map(str::trim);
    let name = fields.next().unwrap_or_default().to_string();
    let uuid = fields.next().unwrap_or_default().to_string();
    let driver = fields.next().unwrap_or_default().to_string();
    if fields.next().is_some() || name.is_empty() || uuid.is_empty() || driver.is_empty() {
        return Err(InterpolationError::new(
            "frame_interpolation_nvidia_output_invalid",
            "nvidia-smi did not return name, UUID, and driver version",
        ));
    }
    if !name.starts_with("NVIDIA GeForce RTX") {
        return Err(InterpolationError::new(
            "frame_interpolation_gpu_unsupported",
            format!("Unsupported GPU: {name}"),
        ));
    }
    Ok(GpuInfo { name, uuid, driver })
}

fn runtime_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("MSGO_FRAME_INTERPOLATION_RUNTIME") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(executable) = env::args_os().next().map(PathBuf::from)
        && executable.is_absolute()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join("frame-interpolation-runtime"));
        candidates.push(
            directory
                .join("third_party")
                .join("frame-interpolation-runtime"),
        );
        if let Some(parent) = directory.parent() {
            candidates.push(
                parent
                    .join("third_party")
                    .join("frame-interpolation-runtime"),
            );
        }
    }
    if let Ok(executable) = env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join("frame-interpolation-runtime"));
        candidates.push(
            directory
                .join("third_party")
                .join("frame-interpolation-runtime"),
        );
        if let Some(parent) = directory.parent() {
            candidates.push(
                parent
                    .join("third_party")
                    .join("frame-interpolation-runtime"),
            );
        }
    }
    if let Ok(directory) = env::current_dir() {
        candidates.push(
            directory
                .join("third_party")
                .join("frame-interpolation-runtime"),
        );
    }
    candidates.dedup();
    candidates
}

fn probe_runtime(cache_root: &Path) -> Result<RuntimeLayout, InterpolationError> {
    let candidates = runtime_candidates();
    let root = candidates
        .iter()
        .find(|candidate| candidate.join("runtime-manifest.json").is_file())
        .cloned()
        .ok_or_else(|| {
            InterpolationError::new(
                "frame_interpolation_runtime_missing",
                format!(
                    "runtime-manifest.json was not found in: {}",
                    candidates
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            )
        })?;
    if !root.as_os_str().to_string_lossy().is_ascii() {
        return Err(InterpolationError::new(
            "frame_interpolation_runtime_path_unsupported",
            format!(
                "VapourSynth R65 requires an ASCII-only runtime path: {}",
                root.display()
            ),
        ));
    }
    let manifest = read_manifest(&root.join("runtime-manifest.json"))?;
    if manifest.vapoursynth != VAPOURSYNTH_VERSION
        || manifest.vs_mlrt != VS_MLRT_VERSION
        || manifest.tensorrt != TENSORRT_VERSION
        || manifest.model != MODEL_NAME
        || manifest.model_sha256 != MODEL_SHA256
    {
        return Err(InterpolationError::new(
            "frame_interpolation_runtime_version_mismatch",
            format!(
                "Expected {VAPOURSYNTH_VERSION}, {VS_MLRT_VERSION}, {TENSORRT_VERSION}, {MODEL_NAME}"
            ),
        ));
    }
    let bin = root.join("bin");
    let python_lib = root.join("lib").join("python3.14");
    let plugin_dir = root.join("vapoursynth").join("plugins");
    let plugin = plugin_dir.join("vstrt_rtx.dll");
    let scripts = root.join("scripts");
    let model = plugin_dir
        .join("models")
        .join("rife")
        .join("rife_v4.25_lite.onnx");
    let engines = cache_root.join("engines");
    for required in [
        bin.join("VSScript.dll"),
        bin.join("libvapoursynth.dll"),
        bin.join("libpython3.14.dll"),
        plugin.clone(),
        plugin_dir.join("vsmlrt-cuda").join("tensorrt_rtx_1_4.dll"),
        scripts.join("vsmlrt.py"),
        model.clone(),
    ] {
        if !required.is_file() {
            return Err(InterpolationError::new(
                "frame_interpolation_runtime_incomplete",
                format!("Required dependency is missing: {}", required.display()),
            ));
        }
    }
    let actual_model_hash = sha256_file(&model)?;
    if actual_model_hash != MODEL_SHA256 {
        return Err(InterpolationError::new(
            "frame_interpolation_model_hash_mismatch",
            format!("RIFE model SHA-256 is {actual_model_hash}"),
        ));
    }
    fs::create_dir_all(&engines).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_engine_cache_unavailable",
            format!("Engine cache could not be created: {error}"),
        )
    })?;
    Ok(RuntimeLayout {
        root,
        bin,
        python_lib,
        plugin_dir,
        plugin,
        scripts,
        model,
        engines,
        manifest,
    })
}

fn read_manifest(path: &Path) -> Result<RuntimeManifest, InterpolationError> {
    let bytes = fs::read(path).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_manifest_unreadable",
            format!("{}: {error}", path.display()),
        )
    })?;
    let json_bytes = bytes
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(bytes.as_slice());
    let value: Value = serde_json::from_slice(json_bytes).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!("{}: {error}", path.display()),
        )
    })?;
    let string = |key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                InterpolationError::new(
                    "frame_interpolation_manifest_invalid",
                    format!("Manifest field is missing: {key}"),
                )
            })
    };
    if value.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            "Only runtime manifest schema 1 is supported",
        ));
    }
    Ok(RuntimeManifest {
        vapoursynth: string("vapoursynth")?,
        vs_mlrt: string("vsMlrt")?,
        tensorrt: string("backend")?,
        model: string("model")?,
        model_sha256: string("modelSha256")?,
        redistributable: value
            .get("redistributable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn sha256_file(path: &Path) -> Result<String, InterpolationError> {
    let bytes = fs::read(path).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_dependency_unreadable",
            format!("{}: {error}", path.display()),
        )
    })?;
    Ok(hex_digest(Sha256::digest(bytes)))
}

fn install_player_script(cache_root: &Path) -> Result<PathBuf, InterpolationError> {
    fs::create_dir_all(cache_root).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_script_cache_unavailable",
            format!("{}: {error}", cache_root.display()),
        )
    })?;
    let path = cache_root.join("rife_player.vpy");
    if fs::read_to_string(&path).is_ok_and(|contents| contents == PLAYER_SCRIPT) {
        return Ok(path);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(cache_root).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_script_cache_unavailable",
            format!("{}: {error}", cache_root.display()),
        )
    })?;
    use std::io::Write as _;
    temporary
        .write_all(PLAYER_SCRIPT.as_bytes())
        .map_err(|error| {
            InterpolationError::new(
                "frame_interpolation_script_cache_unavailable",
                format!("{}: {error}", cache_root.display()),
            )
        })?;
    if path.exists() {
        fs::remove_file(&path).map_err(|error| {
            InterpolationError::new(
                "frame_interpolation_script_cache_unavailable",
                format!("{}: {error}", path.display()),
            )
        })?;
    }
    temporary.persist(&path).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_script_cache_unavailable",
            format!("{}: {}", path.display(), error.error),
        )
    })?;
    Ok(path)
}

fn set_runtime_environment(name: &str, value: &OsStr) -> Result<(), InterpolationError> {
    unsafe {
        env::set_var(name, value);
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let variable_name = OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let value = value
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let status = unsafe { _wputenv_s(variable_name.as_ptr(), value.as_ptr()) };
        if status != 0 {
            return Err(InterpolationError::new(
                "frame_interpolation_environment_invalid",
                format!("{name} could not be applied to the UCRT environment: errno {status}"),
            ));
        }
    }
    Ok(())
}

fn configure_process_environment(layout: &RuntimeLayout) -> Result<(), InterpolationError> {
    let cuda_dir = layout.plugin_dir.join("vsmlrt-cuda");
    let mut paths = vec![layout.bin.clone(), cuda_dir];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    let path = env::join_paths(paths).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_environment_invalid",
            format!("PATH could not be assembled: {error}"),
        )
    })?;
    let python_path = env::join_paths([
        layout.python_lib.clone(),
        layout.python_lib.join("site-packages"),
        layout.scripts.clone(),
    ])
    .map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_environment_invalid",
            format!("PYTHONPATH could not be assembled: {error}"),
        )
    })?;
    // Called once during process boot, before the application starts worker
    // threads. MSYS Python reads UCRT's environment snapshot, so Windows needs
    // both the process environment and UCRT updated before VSScript initializes.
    set_runtime_environment("PATH", &path)?;
    set_runtime_environment("PYTHONHOME", layout.root.as_os_str())?;
    set_runtime_environment("PYTHONPATH", &python_path)?;
    set_runtime_environment("VSSCRIPT_PATH", layout.bin.join("VSScript.dll").as_os_str())?;
    set_runtime_environment("VAPOURSYNTH_PLUGIN_PATH", layout.plugin_dir.as_os_str())?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn preload_vsscript(layout: &RuntimeLayout) -> Result<usize, InterpolationError> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt as _;
    use std::ptr;
    use windows_sys::Win32::System::LibraryLoader::{
        GetDllDirectoryW, GetProcAddress, LoadLibraryW, SetDllDirectoryW,
    };

    const MAX_DLL_DIRECTORY_LENGTH: usize = 32_768;
    const VSSCRIPT_API_VERSION: i32 = (4 << 16) | 1;
    type GetVsscriptApi = unsafe extern "system" fn(i32) -> *const c_void;

    let mut previous_directory = vec![0_u16; MAX_DLL_DIRECTORY_LENGTH];
    let previous_length = unsafe {
        GetDllDirectoryW(
            previous_directory.len() as u32,
            previous_directory.as_mut_ptr(),
        )
    } as usize;
    if previous_length >= previous_directory.len() {
        return Err(InterpolationError::new(
            "frame_interpolation_dll_search_unavailable",
            "The current Windows DLL directory exceeds the supported path length",
        ));
    }
    let previous_directory = if previous_length == 0 {
        None
    } else {
        previous_directory.truncate(previous_length);
        previous_directory.push(0);
        Some(previous_directory)
    };

    let runtime_directory = layout
        .bin
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if unsafe { SetDllDirectoryW(runtime_directory.as_ptr()) } == 0 {
        return Err(InterpolationError::new(
            "frame_interpolation_dll_search_unavailable",
            format!(
                "Windows rejected the isolated runtime DLL directory {}: {}",
                layout.bin.display(),
                std::io::Error::last_os_error()
            ),
        ));
    }

    let load_result = (|| {
        let vsscript_path = layout
            .bin
            .join("VSScript.dll")
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let module = unsafe { LoadLibraryW(vsscript_path.as_ptr()) };
        if module.is_null() {
            return Err(InterpolationError::new(
                "frame_interpolation_vsscript_load_failed",
                format!(
                    "VSScript.dll could not be loaded from {}: {}",
                    layout.bin.display(),
                    std::io::Error::last_os_error()
                ),
            ));
        }
        let symbol = unsafe { GetProcAddress(module, c"getVSScriptAPI".as_ptr().cast()) }
            .ok_or_else(|| {
                InterpolationError::new(
                    "frame_interpolation_vsscript_api_missing",
                    "VSScript.dll does not export getVSScriptAPI",
                )
            })?;
        let get_api = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, GetVsscriptApi>(symbol)
        };
        if unsafe { get_api(VSSCRIPT_API_VERSION) }.is_null() {
            return Err(InterpolationError::new(
                "frame_interpolation_vsscript_initialization_failed",
                "VSScript R65 could not initialize its isolated Python runtime",
            ));
        }
        Ok(module as usize)
    })();

    let restore_path = previous_directory
        .as_ref()
        .map_or(ptr::null(), |path| path.as_ptr());
    if unsafe { SetDllDirectoryW(restore_path) } == 0 {
        let restore_error = std::io::Error::last_os_error();
        return Err(match load_result {
            Ok(_) => InterpolationError::new(
                "frame_interpolation_dll_search_unavailable",
                format!("The Windows DLL directory could not be restored: {restore_error}"),
            ),
            Err(error) => InterpolationError::new(
                error.code,
                format!(
                    "{}; the Windows DLL directory also could not be restored: {restore_error}",
                    error.detail
                ),
            ),
        });
    }
    load_result
}

#[cfg(not(target_os = "windows"))]
fn preload_vsscript(_layout: &RuntimeLayout) -> Result<usize, InterpolationError> {
    Err(InterpolationError::new(
        "frame_interpolation_platform_unsupported",
        "RTX frame interpolation is supported only on Windows",
    ))
}

#[derive(Clone, Debug)]
pub struct PlanRequest {
    pub mode: InterpolationMode,
    pub width: u32,
    pub height: u32,
    pub source_fps: f64,
    pub display_fps: f64,
    pub dynamic_range: Option<String>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_range: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineState {
    Building,
    Cached,
}

impl EngineState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Building => "building",
            Self::Cached => "cached",
        }
    }
}

#[derive(Clone, Debug)]
pub struct InterpolationPlan {
    pub mode: InterpolationMode,
    pub target_fps: u32,
    pub source_fps_num: u32,
    pub source_fps_den: u32,
    pub scale: f64,
    pub video_filter: String,
    pub hwdec: &'static str,
    pub engine_key: String,
    pub engine_directory: PathBuf,
    pub engine_state: EngineState,
    pub model: &'static str,
    pub backend: &'static str,
}

pub fn prepare_plan(request: PlanRequest) -> Result<Option<InterpolationPlan>, InterpolationError> {
    if request.mode == InterpolationMode::Off {
        return Ok(None);
    }
    let context = CONTEXT.get().ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_not_initialized",
            "The frame interpolation runtime has not been initialized",
        )
    })?;
    if let Some(error) = &context.report.failure {
        return Err(error.clone());
    }
    let layout = context.layout.as_ref().ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_runtime_missing",
            "The initialized runtime layout is unavailable",
        )
    })?;
    let gpu = context.gpu.as_ref().ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_gpu_unsupported",
            "The initialized RTX GPU record is unavailable",
        )
    })?;
    let script_path = context.script_path.as_ref().ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_script_cache_unavailable",
            "The installed VapourSynth player script is unavailable",
        )
    })?;
    validate_dimensions(request.width, request.height)?;
    validate_dynamic_range(request.dynamic_range.as_deref())?;
    let (source_fps_num, source_fps_den) = rational_frame_rate(request.source_fps)?;
    let target_fps = target_fps(request.mode, request.display_fps)?;
    if f64::from(target_fps) <= request.source_fps + 0.001 {
        return Err(InterpolationError::new(
            "frame_interpolation_target_not_higher",
            format!(
                "Target {target_fps} FPS must exceed source {:.3} FPS",
                request.source_fps
            ),
        ));
    }
    let matrix = normalize_matrix(request.color_space.as_deref())?;
    let color_range = normalize_color_range(request.color_range.as_deref())?;
    let scale = 1.0;
    let engine_material = format!(
        "gpu={};driver={};tensorrt={};model={};width={};height={};scale={scale:.1};fp16=true",
        gpu.uuid,
        gpu.driver,
        layout.manifest.tensorrt,
        layout.manifest.model_sha256,
        request.width,
        request.height,
    );
    let engine_key = hex_digest(Sha256::digest(engine_material.as_bytes()));
    let engine_directory = layout.engines.join(&engine_key);
    fs::create_dir_all(&engine_directory).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_engine_cache_unavailable",
            format!("{}: {error}", engine_directory.display()),
        )
    })?;
    let engine_state = detect_engine_state(&engine_directory);
    let user_data_json = json!({
        "width": request.width,
        "height": request.height,
        "source_fps_num": source_fps_num,
        "source_fps_den": source_fps_den,
        "target_fps": target_fps,
        "scale": scale,
        "matrix": matrix,
        "color_range": color_range,
        "color_transfer": request.color_transfer,
        "plugin_path": path_for_python(&layout.plugin),
        "scripts_path": path_for_python(&layout.scripts),
        "model_path": path_for_python(&layout.model),
        "engine_path": path_for_python(&engine_directory),
    })
    .to_string();
    let user_data = base64::engine::general_purpose::STANDARD.encode(user_data_json.as_bytes());
    let video_filter = format!(
        "vapoursynth=file={}:buffered-frames=8:concurrent-frames=1:user-data={}",
        fixed_length(script_path),
        fixed_length(&user_data),
    );
    Ok(Some(InterpolationPlan {
        mode: request.mode,
        target_fps,
        source_fps_num,
        source_fps_den,
        scale,
        video_filter,
        hwdec: "d3d11va-copy",
        engine_key,
        engine_directory,
        engine_state,
        model: MODEL_NAME,
        backend: "Backend.TRT_RTX",
    }))
}

pub fn refresh_engine_state(plan: &InterpolationPlan) -> EngineState {
    detect_engine_state(&plan.engine_directory)
}

fn detect_engine_state(directory: &Path) -> EngineState {
    let cached = fs::read_dir(directory).ok().is_some_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "engine")
        })
    });
    if cached {
        EngineState::Cached
    } else {
        EngineState::Building
    }
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), InterpolationError> {
    if width == 0 || height == 0 {
        return Err(InterpolationError::new(
            "frame_interpolation_dimensions_unknown",
            "Source dimensions are missing",
        ));
    }
    if width > 2560 || height > 1440 {
        return Err(InterpolationError::new(
            "frame_interpolation_4k_not_validated",
            "4K interpolation is disabled because RIFE v4.25 Lite scale=0.5 is unavailable in vs-mlrt v15.16",
        ));
    }
    Ok(())
}

fn validate_dynamic_range(value: Option<&str>) -> Result<(), InterpolationError> {
    let normalized = value.unwrap_or_default().trim().to_ascii_lowercase();
    match normalized.as_str() {
        "sdr" | "hdr10" => Ok(()),
        "hlg" => Err(InterpolationError::new(
            "frame_interpolation_hlg_not_validated",
            "HLG interpolation has not completed output validation",
        )),
        "dolby vision" | "dolbyvision" | "dovi" => Err(InterpolationError::new(
            "frame_interpolation_dolby_vision_unsupported",
            "Dolby Vision dynamic metadata cannot be preserved by the first release",
        )),
        "hdr10+" | "hdr10plus" => Err(InterpolationError::new(
            "frame_interpolation_hdr10_plus_unsupported",
            "HDR10+ dynamic metadata cannot be preserved by the first release",
        )),
        _ => Err(InterpolationError::new(
            "frame_interpolation_dynamic_range_unknown",
            format!("Unsupported or missing dynamic range: {normalized}"),
        )),
    }
}

fn target_fps(mode: InterpolationMode, display_fps: f64) -> Result<u32, InterpolationError> {
    if !display_fps.is_finite() || display_fps <= 0.0 {
        return Err(InterpolationError::new(
            "frame_interpolation_display_fps_unknown",
            "The active display refresh rate is unavailable",
        ));
    }
    let target = match mode {
        InterpolationMode::Off => unreachable!("off mode exits before target selection"),
        InterpolationMode::Auto if display_fps >= 118.0 => 120,
        InterpolationMode::Auto if display_fps >= 88.0 => 90,
        InterpolationMode::Auto if display_fps >= 58.0 => 60,
        InterpolationMode::Auto => {
            return Err(InterpolationError::new(
                "frame_interpolation_display_refresh_unsupported",
                format!("Display refresh {:.3} Hz is below 60 Hz", display_fps),
            ));
        }
        InterpolationMode::Fps60 => 60,
        InterpolationMode::Fps90 => 90,
        InterpolationMode::Fps120 => 120,
    };
    if display_fps + 1.0 < f64::from(target) {
        return Err(InterpolationError::new(
            "frame_interpolation_display_refresh_insufficient",
            format!("Target {target} FPS exceeds display refresh {display_fps:.3} Hz"),
        ));
    }
    Ok(target)
}

fn rational_frame_rate(value: f64) -> Result<(u32, u32), InterpolationError> {
    if !value.is_finite() || value <= 0.0 || value > 240.0 {
        return Err(InterpolationError::new(
            "frame_interpolation_source_fps_invalid",
            format!("Invalid source FPS: {value}"),
        ));
    }
    for (known, numerator, denominator) in [
        (23.976, 24_000, 1_001),
        (29.970, 30_000, 1_001),
        (47.952, 48_000, 1_001),
        (59.940, 60_000, 1_001),
        (119.880, 120_000, 1_001),
    ] {
        if (value - known).abs() < 0.01 {
            return Ok((numerator, denominator));
        }
    }
    let denominator = 1_000_u32;
    let numerator = (value * f64::from(denominator)).round() as u32;
    let divisor = greatest_common_divisor(numerator, denominator);
    Ok((numerator / divisor, denominator / divisor))
}

fn normalize_matrix(value: Option<&str>) -> Result<&'static str, InterpolationError> {
    let normalized = value.unwrap_or_default().trim().to_ascii_lowercase();
    match normalized.as_str() {
        "bt709" | "709" => Ok("709"),
        "bt2020nc" | "bt2020ncl" | "2020ncl" | "bt2020" => Ok("2020ncl"),
        "smpte170m" | "bt470bg" | "bt601" | "601" => Ok("470bg"),
        _ => Err(InterpolationError::new(
            "frame_interpolation_color_space_unsupported",
            format!("Unsupported or missing color space: {normalized}"),
        )),
    }
}

fn normalize_color_range(value: Option<&str>) -> Result<&'static str, InterpolationError> {
    let normalized = value.unwrap_or_default().trim().to_ascii_lowercase();
    match normalized.as_str() {
        "tv" | "limited" | "mpeg" => Ok("limited"),
        "pc" | "full" | "jpeg" => Ok("full"),
        _ => Err(InterpolationError::new(
            "frame_interpolation_color_range_unsupported",
            format!("Unsupported or missing color range: {normalized}"),
        )),
    }
}

fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

fn fixed_length(path: impl AsRef<OsStr>) -> String {
    let text = path.as_ref().to_string_lossy();
    format!("%{}%{text}", text.len())
}

fn path_for_python(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip() {
        for mode in [
            InterpolationMode::Off,
            InterpolationMode::Auto,
            InterpolationMode::Fps60,
            InterpolationMode::Fps90,
            InterpolationMode::Fps120,
        ] {
            assert_eq!(InterpolationMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(InterpolationMode::parse("59"), None);
    }

    #[test]
    fn auto_target_tracks_display_refresh() {
        assert_eq!(target_fps(InterpolationMode::Auto, 59.94), Ok(60));
        assert_eq!(target_fps(InterpolationMode::Auto, 90.0), Ok(90));
        assert_eq!(target_fps(InterpolationMode::Auto, 144.0), Ok(120));
    }

    #[test]
    fn explicit_target_rejects_insufficient_display_refresh() {
        let error = target_fps(InterpolationMode::Fps120, 60.0)
            .expect_err("60 Hz display must reject 120 FPS target");
        assert_eq!(
            error.code,
            "frame_interpolation_display_refresh_insufficient"
        );
    }

    #[test]
    fn known_fractional_rates_are_exact() {
        assert_eq!(rational_frame_rate(23.976), Ok((24_000, 1_001)));
        assert_eq!(rational_frame_rate(59.94), Ok((60_000, 1_001)));
        assert_eq!(rational_frame_rate(24.0), Ok((24, 1)));
    }

    #[test]
    fn first_release_rejects_dynamic_metadata_formats() {
        assert!(validate_dynamic_range(Some("SDR")).is_ok());
        assert!(validate_dynamic_range(Some("HDR10")).is_ok());
        assert_eq!(
            validate_dynamic_range(Some("HDR10+"))
                .expect_err("HDR10+ must be rejected")
                .code,
            "frame_interpolation_hdr10_plus_unsupported"
        );
        assert_eq!(
            validate_dynamic_range(Some("Dolby Vision"))
                .expect_err("Dolby Vision must be rejected")
                .code,
            "frame_interpolation_dolby_vision_unsupported"
        );
    }

    #[test]
    fn engine_key_material_changes_for_required_dimensions() {
        let first = format!(
            "gpu={};driver={};tensorrt={};model={};width={};height={};scale=1.0;fp16=true",
            "gpu", "driver", TENSORRT_VERSION, MODEL_SHA256, 1920, 1080
        );
        let second = format!(
            "gpu={};driver={};tensorrt={};model={};width={};height={};scale=1.0;fp16=true",
            "gpu", "driver", TENSORRT_VERSION, MODEL_SHA256, 2560, 1440
        );
        assert_ne!(
            hex_digest(Sha256::digest(first.as_bytes())),
            hex_digest(Sha256::digest(second.as_bytes()))
        );
    }

    #[test]
    fn mpv_fixed_length_uses_utf8_byte_count() {
        let value = std::ffi::OsString::from("C:/媒体/rife.vpy");
        let quoted = fixed_length(&value);
        let text = value.to_string_lossy();
        assert_eq!(quoted, format!("%{}%{text}", text.len()));
    }

    #[test]
    fn runtime_manifest_accepts_utf8_bom() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("runtime-manifest.json");
        let json = format!(
            r#"{{"schema":1,"vapoursynth":"{VAPOURSYNTH_VERSION}","vsMlrt":"{VS_MLRT_VERSION}","backend":"{TENSORRT_VERSION}","model":"{MODEL_NAME}","modelSha256":"{MODEL_SHA256}","redistributable":false}}"#
        );
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(json.as_bytes());
        fs::write(&path, bytes).expect("write manifest");

        let manifest = read_manifest(&path).expect("parse manifest");
        assert_eq!(manifest.vapoursynth, VAPOURSYNTH_VERSION);
        assert!(!manifest.redistributable);
    }
}
