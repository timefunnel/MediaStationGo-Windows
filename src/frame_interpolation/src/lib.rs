use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const RIFE_RUNTIME_ABI: u32 = 3;
const RIFE_BACKEND: &str = "TensorRT-RTX D3D11 P010";
const RIFE_FILTER: &str = "vf_nvofmemc (RIFE mode)";
const RIFE_MODEL: &str = "RIFE v4.26";
const RIFE_TENSORRT_VERSION: &str = "1.4.0.76";
const RIFE_SCALE: &str = "1.0";
const RIFE_PRECISION: &str = "fp16";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InterpolationMode {
    #[default]
    Off,
    Auto,
    X2,
}

impl InterpolationMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "auto" => Some(Self::Auto),
            "2x" | "60" => Some(Self::X2),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::X2 => "2x",
        }
    }

    pub const fn supported_by_native_backend(self) -> bool {
        true
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
    pub runtime_version: Option<String>,
    pub model: Option<String>,
    pub engine_count: usize,
    pub backend: Option<&'static str>,
    pub filter: Option<&'static str>,
    pub failure: Option<InterpolationError>,
    runtime: Option<RuntimeComponents>,
}

#[derive(Clone, Debug)]
struct GpuInfo {
    name: String,
    uuid: String,
    driver: String,
}

#[derive(Clone, Debug)]
struct EngineArtifact {
    width: u32,
    height: u32,
    key: String,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct RuntimeComponents {
    runtime_dll: PathBuf,
    cuda_runtime_dll: PathBuf,
    tensor_rt_version: String,
    model: String,
    model_sha256: String,
    engines: Vec<EngineArtifact>,
}

static REPORT: OnceLock<CapabilityReport> = OnceLock::new();

pub fn initialize(_cache_root: &Path) -> &'static CapabilityReport {
    REPORT.get_or_init(build_capability_report)
}

pub fn capability_report() -> CapabilityReport {
    REPORT.get().cloned().unwrap_or_else(|| CapabilityReport {
        failure: Some(InterpolationError::new(
            "frame_interpolation_not_initialized",
            "The frame interpolation capability probe has not run",
        )),
        ..CapabilityReport::default()
    })
}

fn build_capability_report() -> CapabilityReport {
    match probe_capability() {
        Ok((gpu, runtime)) => CapabilityReport {
            ready: true,
            gpu_name: Some(gpu.name),
            gpu_uuid: Some(gpu.uuid),
            driver_version: Some(gpu.driver),
            runtime_version: Some(format!("TensorRT-RTX {}", runtime.tensor_rt_version)),
            model: Some(runtime.model.clone()),
            engine_count: runtime.engines.len(),
            backend: Some(RIFE_BACKEND),
            filter: Some(RIFE_FILTER),
            failure: None,
            runtime: Some(runtime),
        },
        Err(error) => CapabilityReport {
            failure: Some(error),
            ..CapabilityReport::default()
        },
    }
}

fn probe_capability() -> Result<(GpuInfo, RuntimeComponents), InterpolationError> {
    if !cfg!(target_os = "windows") {
        return Err(InterpolationError::new(
            "frame_interpolation_platform_unsupported",
            "RIFE TensorRT-RTX frame interpolation is supported only on Windows",
        ));
    }
    let gpu = probe_gpu()?;
    let runtime = probe_runtime(&gpu)?;
    probe_d3d_compiler()?;
    Ok((gpu, runtime))
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
    if !name.starts_with("NVIDIA") || !name.contains("RTX") {
        return Err(InterpolationError::new(
            "frame_interpolation_gpu_unsupported",
            format!("Unsupported GPU: {name}"),
        ));
    }
    Ok(GpuInfo { name, uuid, driver })
}

fn probe_runtime(gpu: &GpuInfo) -> Result<RuntimeComponents, InterpolationError> {
    let executable = std::env::current_exe().map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_runtime_location_unavailable",
            format!("The application path is unavailable: {error}"),
        )
    })?;
    let executable_dir = executable.parent().ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_runtime_location_unavailable",
            format!(
                "The application path has no parent: {}",
                executable.display()
            ),
        )
    })?;
    let manifest_path = executable_dir
        .join("frame-interpolation")
        .join("runtime-manifest.json");
    let bytes = std::fs::read(&manifest_path).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_manifest_missing",
            format!("{} could not be read: {error}", manifest_path.display()),
        )
    })?;
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!("{} is invalid JSON: {error}", manifest_path.display()),
        )
    })?;
    if manifest.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            "The RIFE runtime manifest schema is not supported",
        ));
    }
    let manifest_gpu_uuid = manifest_string(&manifest, "gpuUuid")?;
    let manifest_driver = manifest_string(&manifest, "driverVersion")?;
    if manifest_gpu_uuid != gpu.uuid || manifest_driver != gpu.driver {
        return Err(InterpolationError::new(
            "frame_interpolation_engine_cache_mismatch",
            format!(
                "The staged engines target GPU {} driver {}, but the active GPU is {} driver {}",
                manifest_gpu_uuid, manifest_driver, gpu.uuid, gpu.driver
            ),
        ));
    }
    let tensor_rt_version = manifest_string(&manifest, "tensorRtVersion")?;
    let model = manifest_string(&manifest, "model")?;
    let model_sha256 = manifest_string(&manifest, "modelSha256")?;
    let runtime_abi = manifest
        .get("runtimeAbi")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            InterpolationError::new(
                "frame_interpolation_manifest_invalid",
                "The RIFE runtime ABI is missing or invalid",
            )
        })?;
    if tensor_rt_version != RIFE_TENSORRT_VERSION
        || model != RIFE_MODEL
        || runtime_abi != RIFE_RUNTIME_ABI
    {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!(
                "Unsupported runtime tuple TensorRT={} model={} ABI={}",
                tensor_rt_version, model, runtime_abi
            ),
        ));
    }
    let runtime_dll = component_path(executable_dir, &manifest, "runtimeDll")?;
    let cuda_runtime_dll = component_path(executable_dir, &manifest, "cudaRuntimeDll")?;
    let tensor_rt_dll = component_path(executable_dir, &manifest, "tensorRtDll")?;
    for path in [&runtime_dll, &cuda_runtime_dll, &tensor_rt_dll] {
        if !path.is_file() {
            return Err(InterpolationError::new(
                "frame_interpolation_runtime_component_missing",
                format!("Required runtime component is missing: {}", path.display()),
            ));
        }
    }
    probe_runtime_abi(&runtime_dll)?;

    let entries = manifest
        .get("engines")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            InterpolationError::new(
                "frame_interpolation_manifest_invalid",
                "The RIFE engine list is missing",
            )
        })?;
    let engine_dir = manifest_path
        .parent()
        .expect("manifest path has a parent")
        .join("engine-cache");
    let mut engines = Vec::with_capacity(entries.len());
    for entry in entries {
        let width = manifest_u32(entry, "width")?;
        let height = manifest_u32(entry, "height")?;
        let scale = manifest_string(entry, "scale")?;
        let precision = manifest_string(entry, "precision")?;
        let key = manifest_string(entry, "engineKey")?;
        let file = manifest_string(entry, "file")?;
        let expected_hash = manifest_string(entry, "sha256")?;
        if scale != RIFE_SCALE || precision != RIFE_PRECISION {
            return Err(InterpolationError::new(
                "frame_interpolation_manifest_invalid",
                format!("Unsupported engine scale={scale} precision={precision}"),
            ));
        }
        let expected_key = engine_cache_key(
            &gpu.uuid,
            &gpu.driver,
            &tensor_rt_version,
            &model_sha256,
            width,
            height,
        );
        if key != expected_key || file != format!("{key}.engine") {
            return Err(InterpolationError::new(
                "frame_interpolation_engine_cache_mismatch",
                format!("The {width}x{height} engine cache key is invalid"),
            ));
        }
        let path = engine_dir.join(&file);
        if !path.is_file() {
            return Err(InterpolationError::new(
                "frame_interpolation_engine_missing",
                format!(
                    "The {width}x{height} RIFE engine is missing: {}",
                    path.display()
                ),
            ));
        }
        let actual_hash = hash_file(&path)?;
        if !actual_hash.eq_ignore_ascii_case(&expected_hash) {
            return Err(InterpolationError::new(
                "frame_interpolation_engine_corrupt",
                format!("The {width}x{height} RIFE engine hash does not match its manifest"),
            ));
        }
        engines.push(EngineArtifact {
            width,
            height,
            key,
            path,
        });
    }
    if engines.is_empty() {
        return Err(InterpolationError::new(
            "frame_interpolation_engine_missing",
            "The RIFE runtime does not contain any validated engines",
        ));
    }
    Ok(RuntimeComponents {
        runtime_dll,
        cuda_runtime_dll,
        tensor_rt_version,
        model,
        model_sha256,
        engines,
    })
}

fn manifest_string(manifest: &Value, name: &'static str) -> Result<String, InterpolationError> {
    manifest
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            InterpolationError::new(
                "frame_interpolation_manifest_invalid",
                format!("The RIFE runtime manifest field {name} is missing"),
            )
        })
}

fn manifest_u32(manifest: &Value, name: &'static str) -> Result<u32, InterpolationError> {
    manifest
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            InterpolationError::new(
                "frame_interpolation_manifest_invalid",
                format!("The RIFE runtime manifest field {name} is invalid"),
            )
        })
}

fn component_path(
    executable_dir: &Path,
    manifest: &Value,
    name: &'static str,
) -> Result<PathBuf, InterpolationError> {
    let file = manifest_string(manifest, name)?;
    let path = Path::new(&file);
    if path.file_name().and_then(|value| value.to_str()) != Some(file.as_str()) {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!("Runtime component {name} must be a file name"),
        ));
    }
    Ok(executable_dir.join(file))
}

fn engine_cache_key(
    gpu_uuid: &str,
    driver: &str,
    tensor_rt_version: &str,
    model_sha256: &str,
    width: u32,
    height: u32,
) -> String {
    let material = format!(
        "gpu_uuid={gpu_uuid}\ndriver={driver}\ntensorrt={tensor_rt_version}\nmodel_sha256={model_sha256}\nwidth={width}\nheight={height}\nscale={RIFE_SCALE}\nprecision={RIFE_PRECISION}"
    );
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

fn hash_file(path: &Path) -> Result<String, InterpolationError> {
    let mut file = File::open(path).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_engine_missing",
            format!("{} could not be opened: {error}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            InterpolationError::new(
                "frame_interpolation_engine_corrupt",
                format!("{} could not be read: {error}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(target_os = "windows")]
fn probe_runtime_abi(path: &Path) -> Result<(), InterpolationError> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    type AbiVersion = unsafe extern "C" fn() -> u32;
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        return Err(InterpolationError::new(
            "frame_interpolation_runtime_load_failed",
            format!(
                "{} could not be loaded: {}",
                path.display(),
                std::io::Error::last_os_error()
            ),
        ));
    }
    let address = unsafe { GetProcAddress(module, c"rife_runtime_abi_version".as_ptr().cast()) };
    let result = if let Some(address) = address {
        let function: AbiVersion = unsafe { std::mem::transmute(address) };
        let version = unsafe { function() };
        if version == RIFE_RUNTIME_ABI {
            Ok(())
        } else {
            Err(InterpolationError::new(
                "frame_interpolation_runtime_abi_mismatch",
                format!(
                    "RIFE runtime ABI {version} does not match required ABI {RIFE_RUNTIME_ABI}"
                ),
            ))
        }
    } else {
        Err(InterpolationError::new(
            "frame_interpolation_runtime_abi_mismatch",
            "rife_runtime_abi_version is missing from the runtime DLL",
        ))
    };
    unsafe { FreeLibrary(module) };
    result
}

#[cfg(not(target_os = "windows"))]
fn probe_runtime_abi(_path: &Path) -> Result<(), InterpolationError> {
    Err(InterpolationError::new(
        "frame_interpolation_platform_unsupported",
        "RIFE TensorRT-RTX frame interpolation is supported only on Windows",
    ))
}

#[cfg(target_os = "windows")]
fn probe_d3d_compiler() -> Result<(), InterpolationError> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

    let library_name = OsStr::new("d3dcompiler_47.dll")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let module = unsafe { LoadLibraryW(library_name.as_ptr()) };
    if module.is_null() {
        return Err(InterpolationError::new(
            "frame_interpolation_d3d_compiler_unavailable",
            format!(
                "d3dcompiler_47.dll could not be loaded: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    unsafe { FreeLibrary(module) };
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn probe_d3d_compiler() -> Result<(), InterpolationError> {
    Err(InterpolationError::new(
        "frame_interpolation_platform_unsupported",
        "RIFE TensorRT-RTX frame interpolation is supported only on Windows",
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

#[derive(Clone, Debug)]
pub struct InterpolationPlan {
    pub mode: InterpolationMode,
    pub source_width: u32,
    pub source_height: u32,
    pub target_fps: f64,
    pub target_fps_num: u32,
    pub target_fps_den: u32,
    pub source_fps_num: u32,
    pub source_fps_den: u32,
    pub video_filter: String,
    pub hwdec: &'static str,
    pub backend: &'static str,
    pub runtime_version: String,
    pub model: String,
    pub model_sha256: String,
    pub engine_key: String,
    pub engine_path: PathBuf,
    pub scale: &'static str,
    pub precision: &'static str,
}

pub fn prepare_plan(request: PlanRequest) -> Result<Option<InterpolationPlan>, InterpolationError> {
    if request.mode == InterpolationMode::Off {
        return Ok(None);
    }
    let report = REPORT.get().ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_not_initialized",
            "The frame interpolation capability probe has not run",
        )
    })?;
    build_plan(report, request).map(Some)
}

fn build_plan(
    report: &CapabilityReport,
    request: PlanRequest,
) -> Result<InterpolationPlan, InterpolationError> {
    if let Some(error) = &report.failure {
        return Err(error.clone());
    }
    if !report.ready {
        return Err(InterpolationError::new(
            "frame_interpolation_runtime_unavailable",
            "The RIFE TensorRT-RTX capability probe did not report a usable runtime",
        ));
    }
    let runtime = report.runtime.as_ref().ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_runtime_unavailable",
            "The RIFE runtime component paths are unavailable",
        )
    })?;
    validate_dimensions(request.width, request.height)?;
    validate_dynamic_range(request.dynamic_range.as_deref())?;
    normalize_matrix(request.color_space.as_deref())?;
    normalize_color_range(request.color_range.as_deref())?;
    validate_transfer(
        request.dynamic_range.as_deref(),
        request.color_transfer.as_deref(),
    )?;
    let (source_fps_num, source_fps_den) = rational_frame_rate(request.source_fps)?;
    let (target_fps_num, target_fps_den) = target_frame_rate(
        request.mode,
        request.display_fps,
        source_fps_num,
        source_fps_den,
    )?;
    let target_fps = f64::from(target_fps_num) / f64::from(target_fps_den);
    if target_fps <= request.source_fps + 0.001 {
        return Err(InterpolationError::new(
            "frame_interpolation_target_not_higher",
            format!(
                "Target {target_fps:.3} FPS must exceed source {:.3} FPS",
                request.source_fps
            ),
        ));
    }
    let engine = runtime
        .engines
        .iter()
        .find(|engine| engine.width == request.width && engine.height == request.height)
        .ok_or_else(|| {
            let supported = runtime
                .engines
                .iter()
                .map(|engine| format!("{}x{}", engine.width, engine.height))
                .collect::<Vec<_>>()
                .join(", ");
            InterpolationError::new(
                "frame_interpolation_engine_shape_unsupported",
                format!(
                    "No exact RIFE engine exists for {}x{}; validated shapes: {supported}",
                    request.width, request.height
                ),
            )
        })?;
    let video_filter = format!(
        "nvofmemc=rife=yes:rife-source-width={}:rife-source-height={}:rife-runtime-dll={}:rife-engine={}:rife-cudart={}",
        request.width,
        request.height,
        mpv_filter_path(&runtime.runtime_dll)?,
        mpv_filter_path(&engine.path)?,
        mpv_filter_path(&runtime.cuda_runtime_dll)?,
    );
    Ok(InterpolationPlan {
        mode: request.mode,
        source_width: request.width,
        source_height: request.height,
        target_fps,
        target_fps_num,
        target_fps_den,
        source_fps_num,
        source_fps_den,
        video_filter,
        hwdec: "d3d11va",
        backend: RIFE_BACKEND,
        runtime_version: format!("TensorRT-RTX {}", runtime.tensor_rt_version),
        model: runtime.model.clone(),
        model_sha256: runtime.model_sha256.clone(),
        engine_key: engine.key.clone(),
        engine_path: engine.path.clone(),
        scale: RIFE_SCALE,
        precision: RIFE_PRECISION,
    })
}

fn mpv_filter_path(path: &Path) -> Result<String, InterpolationError> {
    let value = path.to_str().ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_runtime_path_invalid",
            format!("Runtime path is not valid UTF-8: {}", path.display()),
        )
    })?;
    Ok(format!("%{}%{value}", value.len()))
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), InterpolationError> {
    if width == 0 || height == 0 {
        return Err(InterpolationError::new(
            "frame_interpolation_dimensions_unknown",
            "Source dimensions are missing",
        ));
    }
    if width > 3840 || height > 2160 {
        return Err(InterpolationError::new(
            "frame_interpolation_dimensions_unsupported",
            format!("RIFE interpolation supports at most 3840x2160, received {width}x{height}"),
        ));
    }
    Ok(())
}

fn validate_dynamic_range(value: Option<&str>) -> Result<(), InterpolationError> {
    let normalized = value.unwrap_or_default().trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "sdr" | "hdr10" => Ok(()),
        "hlg" => Err(InterpolationError::new(
            "frame_interpolation_hlg_not_validated",
            "HLG interpolation has not completed output validation",
        )),
        "dolby vision" | "dolbyvision" | "dovi" => Err(InterpolationError::new(
            "frame_interpolation_dolby_vision_unsupported",
            "Dolby Vision dynamic metadata cannot be preserved",
        )),
        "hdr10+" | "hdr10plus" => Err(InterpolationError::new(
            "frame_interpolation_hdr10_plus_unsupported",
            "HDR10+ dynamic metadata cannot be preserved",
        )),
        _ => Err(InterpolationError::new(
            "frame_interpolation_dynamic_range_unknown",
            format!("Unsupported or missing dynamic range: {normalized}"),
        )),
    }
}

fn target_frame_rate(
    mode: InterpolationMode,
    display_fps: f64,
    source_num: u32,
    source_den: u32,
) -> Result<(u32, u32), InterpolationError> {
    if !display_fps.is_finite() || display_fps <= 0.0 {
        return Err(InterpolationError::new(
            "frame_interpolation_display_fps_unknown",
            "The active display refresh rate is unavailable",
        ));
    }
    match mode {
        InterpolationMode::Off => unreachable!("off mode exits before target selection"),
        InterpolationMode::Auto | InterpolationMode::X2 => {}
    }
    let doubled = source_num.checked_mul(2).ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_source_fps_invalid",
            "The doubled source frame rate overflows its rational representation",
        )
    })?;
    let divisor = greatest_common_divisor(doubled, source_den);
    let target = (doubled / divisor, source_den / divisor);
    let target_value = f64::from(target.0) / f64::from(target.1);
    if display_fps + 1.0 < target_value {
        return Err(InterpolationError::new(
            "frame_interpolation_display_refresh_insufficient",
            format!("Target {target_value:.3} FPS exceeds display refresh {display_fps:.3} Hz"),
        ));
    }
    Ok(target)
}

fn validate_transfer(
    dynamic_range: Option<&str>,
    transfer: Option<&str>,
) -> Result<(), InterpolationError> {
    let range = dynamic_range
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let transfer = transfer.unwrap_or_default().trim().to_ascii_lowercase();
    if range == "hdr10"
        && !transfer.is_empty()
        && !matches!(transfer.as_str(), "pq" | "smpte2084" | "st2084")
    {
        return Err(InterpolationError::new(
            "frame_interpolation_hdr10_transfer_invalid",
            format!("HDR10 requires PQ/ST2084 transfer metadata, received {transfer}"),
        ));
    }
    Ok(())
}

fn rational_frame_rate(value: f64) -> Result<(u32, u32), InterpolationError> {
    if !value.is_finite() || value <= 0.0 || value > 240.0 {
        return Err(InterpolationError::new(
            "frame_interpolation_source_fps_invalid",
            format!("Invalid source FPS: {value}"),
        ));
    }
    if !(20.0..=30.001).contains(&value) {
        return Err(InterpolationError::new(
            "frame_interpolation_source_fps_unsupported",
            format!("Strict x2 RIFE supports 20-30 FPS sources, received {value:.3}"),
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

fn normalize_matrix(value: Option<&str>) -> Result<(), InterpolationError> {
    let normalized = value.unwrap_or_default().trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "bt709" | "709" | "bt2020nc" | "bt2020ncl" | "2020ncl" | "bt2020" | "smpte170m"
        | "bt470bg" | "bt601" | "601" => Ok(()),
        _ => Err(InterpolationError::new(
            "frame_interpolation_color_space_unsupported",
            format!("Unsupported or missing color space: {normalized}"),
        )),
    }
}

fn normalize_color_range(value: Option<&str>) -> Result<(), InterpolationError> {
    let normalized = value.unwrap_or_default().trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "tv" | "limited" | "mpeg" | "pc" | "full" | "jpeg" => Ok(()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_report() -> CapabilityReport {
        let engines = [(1920, 1080), (2304, 1296), (2560, 1440), (3840, 2160)]
            .into_iter()
            .map(|(width, height)| EngineArtifact {
                width,
                height,
                key: format!("engine-{width}x{height}"),
                path: PathBuf::from(format!("/runtime/{width}x{height}.engine")),
            })
            .collect();
        CapabilityReport {
            ready: true,
            runtime_version: Some(format!("TensorRT-RTX {RIFE_TENSORRT_VERSION}")),
            model: Some(RIFE_MODEL.to_string()),
            engine_count: 4,
            backend: Some(RIFE_BACKEND),
            filter: Some(RIFE_FILTER),
            runtime: Some(RuntimeComponents {
                runtime_dll: PathBuf::from("/runtime/rife_runtime.dll"),
                cuda_runtime_dll: PathBuf::from("/runtime/cudart64_12.dll"),
                tensor_rt_version: RIFE_TENSORRT_VERSION.to_string(),
                model: RIFE_MODEL.to_string(),
                model_sha256: "model-sha256".to_string(),
                engines,
            }),
            ..CapabilityReport::default()
        }
    }

    fn request(mode: InterpolationMode, width: u32, height: u32, source_fps: f64) -> PlanRequest {
        PlanRequest {
            mode,
            width,
            height,
            source_fps,
            display_fps: 60.0,
            dynamic_range: Some("SDR".to_string()),
            color_space: Some("bt709".to_string()),
            color_transfer: Some("bt709".to_string()),
            color_range: Some("limited".to_string()),
        }
    }

    #[test]
    fn modes_round_trip() {
        for mode in [
            InterpolationMode::Off,
            InterpolationMode::Auto,
            InterpolationMode::X2,
        ] {
            assert_eq!(InterpolationMode::parse(mode.as_str()), Some(mode));
            assert!(mode.supported_by_native_backend());
        }
        assert_eq!(InterpolationMode::parse("60"), Some(InterpolationMode::X2));
        assert_eq!(InterpolationMode::parse("59"), None);
    }

    #[test]
    fn auto_and_explicit_x2_use_rife_filter() {
        for mode in [InterpolationMode::Auto, InterpolationMode::X2] {
            let plan = build_plan(&ready_report(), request(mode, 3840, 2160, 24.0))
                .expect("4K 24 to 48 plan");
            assert_eq!(plan.target_fps, 48.0);
            assert_eq!((plan.target_fps_num, plan.target_fps_den), (48, 1));
            assert_eq!(plan.hwdec, "d3d11va");
            assert_eq!(plan.backend, RIFE_BACKEND);
            assert!(plan.video_filter.starts_with("nvofmemc=rife=yes:"));
            assert!(plan.video_filter.contains("rife-runtime-dll="));
            assert!(plan.video_filter.contains("rife-engine="));
            assert!(plan.video_filter.contains("rife-cudart="));
            assert_eq!(plan.engine_key, "engine-3840x2160");
            assert_eq!(plan.scale, "1.0");
            assert_eq!(plan.precision, "fp16");
        }
    }

    #[test]
    fn fractional_sources_are_doubled_exactly() {
        let mut input = request(InterpolationMode::X2, 1920, 1080, 23.976);
        input.display_fps = 48.0;
        let plan = build_plan(&ready_report(), input).expect("fractional plan");
        assert_eq!((plan.source_fps_num, plan.source_fps_den), (24_000, 1_001));
        assert_eq!((plan.target_fps_num, plan.target_fps_den), (48_000, 1_001));
        assert!((plan.target_fps - 47.952).abs() < 0.001);
    }

    #[test]
    fn thirty_fps_targets_sixty() {
        let plan = build_plan(
            &ready_report(),
            request(InterpolationMode::X2, 2560, 1440, 30.0),
        )
        .expect("30 to 60 plan");
        assert_eq!((plan.target_fps_num, plan.target_fps_den), (60, 1));
    }

    #[test]
    fn first_release_accepts_4k_and_rejects_above_4k() {
        assert!(validate_dimensions(3840, 2160).is_ok());
        assert_eq!(
            validate_dimensions(7680, 4320)
                .expect_err("8K must fail")
                .code,
            "frame_interpolation_dimensions_unsupported"
        );
    }

    #[test]
    fn exact_engine_shape_is_required_without_scaling() {
        let error = build_plan(
            &ready_report(),
            request(InterpolationMode::X2, 1920, 800, 24.0),
        )
        .expect_err("an unvalidated engine shape must fail");
        assert_eq!(error.code, "frame_interpolation_engine_shape_unsupported");
    }

    #[test]
    fn sdr_and_hdr10_are_accepted_while_dynamic_metadata_formats_are_rejected() {
        assert!(validate_dynamic_range(Some("SDR")).is_ok());
        assert!(validate_dynamic_range(Some("HDR10")).is_ok());
        assert_eq!(
            validate_dynamic_range(Some("HLG"))
                .expect_err("HLG must fail until validated")
                .code,
            "frame_interpolation_hlg_not_validated"
        );
        assert_eq!(
            validate_dynamic_range(Some("HDR10+"))
                .expect_err("HDR10+ must fail")
                .code,
            "frame_interpolation_hdr10_plus_unsupported"
        );
        assert_eq!(
            validate_dynamic_range(Some("Dolby Vision"))
                .expect_err("Dolby Vision must fail")
                .code,
            "frame_interpolation_dolby_vision_unsupported"
        );
    }

    #[test]
    fn missing_server_color_metadata_is_deferred_to_native_frames() {
        let mut input = request(InterpolationMode::X2, 1920, 1080, 24.0);
        input.dynamic_range = None;
        input.color_space = None;
        input.color_transfer = None;
        input.color_range = None;
        let plan =
            build_plan(&ready_report(), input).expect("native filter validates decoded frames");
        assert_eq!(plan.hwdec, "d3d11va");
    }

    #[test]
    fn hdr10_requires_pq_when_transfer_metadata_is_present() {
        let mut input = request(InterpolationMode::X2, 3840, 2160, 24.0);
        input.dynamic_range = Some("HDR10".to_string());
        input.color_space = Some("bt2020nc".to_string());
        input.color_transfer = Some("pq".to_string());
        assert!(build_plan(&ready_report(), input.clone()).is_ok());

        input.color_transfer = Some("hlg".to_string());
        assert_eq!(
            build_plan(&ready_report(), input)
                .expect_err("HDR10 with HLG transfer must fail")
                .code,
            "frame_interpolation_hdr10_transfer_invalid"
        );
    }

    #[test]
    fn supported_source_rate_bounds_are_explicit() {
        assert_eq!(rational_frame_rate(23.976), Ok((24_000, 1_001)));
        assert_eq!(rational_frame_rate(24.0), Ok((24, 1)));
        assert_eq!(rational_frame_rate(30.0), Ok((30, 1)));
        assert_eq!(
            rational_frame_rate(19.0)
                .expect_err("below supported range")
                .code,
            "frame_interpolation_source_fps_unsupported"
        );
        assert_eq!(
            rational_frame_rate(30.1)
                .expect_err("above supported range")
                .code,
            "frame_interpolation_source_fps_unsupported"
        );
    }

    #[test]
    fn display_refresh_must_fit_the_doubled_rate() {
        let mut input = request(InterpolationMode::X2, 1920, 1080, 30.0);
        input.display_fps = 50.0;
        assert_eq!(
            build_plan(&ready_report(), input)
                .expect_err("50 Hz display cannot present 60 fps")
                .code,
            "frame_interpolation_display_refresh_insufficient"
        );
    }
}
