use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::ffi::OsString;
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

const RIFE_RUNTIME_ABI: u32 = 5;
const RIFE_BACKEND: &str = "TensorRT-RTX D3D11 P010";
const RIFE_FILTER: &str = "vf_nvofmemc (RIFE mode)";
const RIFE_TENSORRT_VERSION: &str = "1.4.0.76";
const RIFE_PRECISION: &str = "fp16";
const RIFE_PROFILE_MAX: u32 = 16_384;

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InterpolationModel {
    #[default]
    RifeV426,
    RifeV426Scale05,
    RifeV425Lite,
}

impl InterpolationModel {
    pub const ALL: [Self; 3] = [Self::RifeV426, Self::RifeV426Scale05, Self::RifeV425Lite];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "rife-v4.26" => Some(Self::RifeV426),
            "rife-v4.26-scale0.5" => Some(Self::RifeV426Scale05),
            "rife-v4.25-lite" => Some(Self::RifeV425Lite),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RifeV426 => "rife-v4.26",
            Self::RifeV426Scale05 => "rife-v4.26-scale0.5",
            Self::RifeV425Lite => "rife-v4.25-lite",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::RifeV426 => "RIFE v4.26",
            Self::RifeV426Scale05 => "RIFE v4.26 (scale=0.5)",
            Self::RifeV425Lite => "RIFE v4.25 Lite",
        }
    }

    pub const fn scale(self) -> &'static str {
        match self {
            Self::RifeV426Scale05 => "0.5",
            Self::RifeV426 | Self::RifeV425Lite => "1.0",
        }
    }

    pub const fn shape_alignment(self) -> u32 {
        match self {
            Self::RifeV426 => 64,
            Self::RifeV426Scale05 | Self::RifeV425Lite => 128,
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
    pub runtime_version: Option<String>,
    pub model: Option<String>,
    pub models: Vec<ModelCapability>,
    pub engine_count: usize,
    pub backend: Option<&'static str>,
    pub filter: Option<&'static str>,
    pub failure: Option<InterpolationError>,
    runtime: Option<RuntimeComponents>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCapability {
    pub id: &'static str,
    pub name: &'static str,
    pub engine_count: usize,
}

#[derive(Clone, Debug)]
struct GpuInfo {
    name: String,
    uuid: String,
    driver: String,
}

#[derive(Clone, Debug)]
struct EngineArtifact {
    key: String,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct EngineProfile {
    min_width: u32,
    min_height: u32,
    opt_width: u32,
    opt_height: u32,
    max_width: u32,
    max_height: u32,
}

#[derive(Clone, Debug)]
struct ModelRuntime {
    model: InterpolationModel,
    model_sha256: String,
    onnx_path: PathBuf,
    profile: EngineProfile,
    engine_key: String,
    engine: Option<EngineArtifact>,
    cached_engine_failure: Option<InterpolationError>,
}

#[derive(Clone, Debug)]
struct RuntimeComponents {
    runtime_dll: PathBuf,
    cuda_runtime_dll: PathBuf,
    engine_builder: PathBuf,
    tensor_rt_version: String,
    cache_dir: PathBuf,
    models: Vec<ModelRuntime>,
}

static REPORT: OnceLock<CapabilityReport> = OnceLock::new();
static ENGINE_BUILD_LOCK: Mutex<()> = Mutex::new(());

pub fn initialize(cache_root: &Path) -> &'static CapabilityReport {
    let cache_root = cache_root.to_path_buf();
    REPORT.get_or_init(|| build_capability_report(&cache_root))
}

pub fn capability_report() -> CapabilityReport {
    let mut report = REPORT.get().cloned().unwrap_or_else(|| CapabilityReport {
        failure: Some(InterpolationError::new(
            "frame_interpolation_not_initialized",
            "The frame interpolation capability probe has not run",
        )),
        ..CapabilityReport::default()
    });
    if let Some(runtime) = &report.runtime {
        for capability in &mut report.models {
            let Some(model) = runtime
                .models
                .iter()
                .find(|model| model.model.as_str() == capability.id)
            else {
                continue;
            };
            let engine_path = runtime
                .cache_dir
                .join(format!("{}.engine", model.engine_key));
            let metadata_path = runtime.cache_dir.join(format!("{}.json", model.engine_key));
            capability.engine_count = usize::from(matches!(
                validate_cached_engine(&engine_path, &metadata_path, &model.engine_key),
                Ok(Some(_))
            ));
        }
        report.engine_count = report.models.iter().map(|model| model.engine_count).sum();
    }
    report
}

fn build_capability_report(cache_root: &Path) -> CapabilityReport {
    match probe_capability(cache_root) {
        Ok((gpu, runtime)) => {
            let models = runtime
                .models
                .iter()
                .map(|model| ModelCapability {
                    id: model.model.as_str(),
                    name: model.model.display_name(),
                    engine_count: usize::from(model.engine.is_some()),
                })
                .collect::<Vec<_>>();
            let model_names = models
                .iter()
                .map(|model| model.name)
                .collect::<Vec<_>>()
                .join(", ");
            let engine_count = models.iter().map(|model| model.engine_count).sum();
            CapabilityReport {
                ready: true,
                gpu_name: Some(gpu.name),
                gpu_uuid: Some(gpu.uuid),
                driver_version: Some(gpu.driver),
                runtime_version: Some(format!("TensorRT-RTX {}", runtime.tensor_rt_version)),
                model: Some(model_names),
                models,
                engine_count,
                backend: Some(RIFE_BACKEND),
                filter: Some(RIFE_FILTER),
                failure: None,
                runtime: Some(runtime),
            }
        }
        Err(error) => CapabilityReport {
            failure: Some(error),
            ..CapabilityReport::default()
        },
    }
}

fn probe_capability(cache_root: &Path) -> Result<(GpuInfo, RuntimeComponents), InterpolationError> {
    if !cfg!(target_os = "windows") {
        return Err(InterpolationError::new(
            "frame_interpolation_platform_unsupported",
            "RIFE TensorRT-RTX frame interpolation is supported only on Windows",
        ));
    }
    let gpu = probe_gpu()?;
    let runtime = probe_runtime(&gpu, cache_root)?;
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

fn probe_runtime(
    gpu: &GpuInfo,
    cache_root: &Path,
) -> Result<RuntimeComponents, InterpolationError> {
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
    let runtime_dir = executable_dir.join("frame-interpolation");
    let manifest_path = runtime_dir.join("runtime-manifest.json");
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
    if manifest.get("schema").and_then(Value::as_u64) != Some(3) {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            "The RIFE runtime manifest schema is not supported",
        ));
    }
    let tensor_rt_version = manifest_string(&manifest, "tensorRtVersion")?;
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
    if tensor_rt_version != RIFE_TENSORRT_VERSION || runtime_abi != RIFE_RUNTIME_ABI {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!(
                "Unsupported runtime tuple TensorRT={} ABI={}",
                tensor_rt_version, runtime_abi
            ),
        ));
    }
    let runtime_dll = component_path(executable_dir, &manifest, "runtimeDll")?;
    let cuda_runtime_dll = component_path(executable_dir, &manifest, "cudaRuntimeDll")?;
    let tensor_rt_dll = component_path(executable_dir, &manifest, "tensorRtDll")?;
    let onnx_parser_dll = component_path(executable_dir, &manifest, "onnxParserDll")?;
    let engine_builder = component_path(executable_dir, &manifest, "engineBuilder")?;
    for path in [
        &runtime_dll,
        &cuda_runtime_dll,
        &tensor_rt_dll,
        &onnx_parser_dll,
        &engine_builder,
    ] {
        if !path.is_file() {
            return Err(InterpolationError::new(
                "frame_interpolation_runtime_component_missing",
                format!("Required runtime component is missing: {}", path.display()),
            ));
        }
    }
    probe_runtime_abi(&runtime_dll)?;
    let cache_dir = cache_root.join("engine-cache");
    std::fs::create_dir_all(&cache_dir).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_engine_cache_unavailable",
            format!("{} could not be created: {error}", cache_dir.display()),
        )
    })?;
    let builder_sha256 = hash_file(
        &engine_builder,
        "frame_interpolation_runtime_component_missing",
        "frame_interpolation_runtime_component_corrupt",
    )?;

    let entries = manifest
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            InterpolationError::new(
                "frame_interpolation_manifest_invalid",
                "The RIFE model list is missing",
            )
        })?;
    let mut models = Vec::with_capacity(entries.len());
    for entry in entries {
        let model = parse_model_runtime(
            entry,
            gpu,
            &tensor_rt_version,
            &builder_sha256,
            &runtime_dir,
            &cache_dir,
        )?;
        if models
            .iter()
            .any(|existing: &ModelRuntime| existing.model == model.model)
        {
            return Err(InterpolationError::new(
                "frame_interpolation_manifest_invalid",
                format!("Duplicate RIFE model entry: {}", model.model.as_str()),
            ));
        }
        models.push(model);
    }
    for required in InterpolationModel::ALL {
        if !models.iter().any(|model| model.model == required) {
            return Err(InterpolationError::new(
                "frame_interpolation_model_unavailable",
                format!(
                    "Required RIFE model is missing: {}",
                    required.display_name()
                ),
            ));
        }
    }
    models.sort_by_key(|model| {
        InterpolationModel::ALL
            .iter()
            .position(|candidate| *candidate == model.model)
            .unwrap_or(usize::MAX)
    });
    let active_keys = models
        .iter()
        .map(|model| engine_cache_key(gpu, &tensor_rt_version, &builder_sha256, model))
        .collect::<Vec<_>>();
    cleanup_obsolete_engine_cache(&cache_dir, &active_keys)?;
    Ok(RuntimeComponents {
        runtime_dll,
        cuda_runtime_dll,
        engine_builder,
        tensor_rt_version,
        cache_dir,
        models,
    })
}

fn parse_model_runtime(
    entry: &Value,
    gpu: &GpuInfo,
    tensor_rt_version: &str,
    builder_sha256: &str,
    runtime_dir: &Path,
    cache_dir: &Path,
) -> Result<ModelRuntime, InterpolationError> {
    let model_id = manifest_string(entry, "id")?;
    let model = InterpolationModel::parse(&model_id).ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!("Unsupported RIFE model id: {model_id}"),
        )
    })?;
    let model_name = manifest_string(entry, "name")?;
    if model_name != model.display_name() {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!("RIFE model name does not match id {model_id}"),
        ));
    }
    let onnx_file = manifest_string(entry, "onnxFile")?;
    if Path::new(&onnx_file)
        .file_name()
        .and_then(|value| value.to_str())
        != Some(onnx_file.as_str())
    {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!("RIFE ONNX path must be a file name: {onnx_file}"),
        ));
    }
    let onnx_path = runtime_dir.join("models").join(&onnx_file);
    if !onnx_path.is_file() {
        return Err(InterpolationError::new(
            "frame_interpolation_model_unavailable",
            format!("Required RIFE ONNX is missing: {}", onnx_path.display()),
        ));
    }
    let model_sha256 = manifest_string(entry, "onnxSha256")?;
    let actual_model_sha256 = hash_file(
        &onnx_path,
        "frame_interpolation_model_unavailable",
        "frame_interpolation_model_corrupt",
    )?;
    if !actual_model_sha256.eq_ignore_ascii_case(&model_sha256) {
        return Err(InterpolationError::new(
            "frame_interpolation_model_corrupt",
            format!(
                "{} ONNX hash does not match the manifest",
                model.display_name()
            ),
        ));
    }
    let scale = manifest_string(entry, "scale")?;
    let precision = manifest_string(entry, "precision")?;
    let shape_alignment = manifest_u32(entry, "shapeAlignment")?;
    if scale != model.scale()
        || precision != RIFE_PRECISION
        || shape_alignment != model.shape_alignment()
    {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!(
                "{} has an invalid scale, precision, or shape alignment",
                model.display_name()
            ),
        ));
    }
    let profile_value = entry.get("profile").ok_or_else(|| {
        InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!("{} dynamic profile is missing", model.display_name()),
        )
    })?;
    let profile = EngineProfile {
        min_width: manifest_u32(profile_value, "minWidth")?,
        min_height: manifest_u32(profile_value, "minHeight")?,
        opt_width: manifest_u32(profile_value, "optWidth")?,
        opt_height: manifest_u32(profile_value, "optHeight")?,
        max_width: manifest_u32(profile_value, "maxWidth")?,
        max_height: manifest_u32(profile_value, "maxHeight")?,
    };
    validate_profile(model, &profile)?;
    let mut provisional = ModelRuntime {
        model,
        model_sha256,
        onnx_path,
        profile,
        engine_key: String::new(),
        engine: None,
        cached_engine_failure: None,
    };
    let key = engine_cache_key(gpu, tensor_rt_version, builder_sha256, &provisional);
    provisional.engine_key.clone_from(&key);
    let engine_path = cache_dir.join(format!("{key}.engine"));
    let metadata_path = cache_dir.join(format!("{key}.json"));
    let (engine, cached_engine_failure) =
        match validate_cached_engine(&engine_path, &metadata_path, &key) {
            Ok(engine) => (engine, None),
            Err(error) => (None, Some(error)),
        };
    Ok(ModelRuntime {
        engine,
        cached_engine_failure,
        ..provisional
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
    gpu: &GpuInfo,
    tensor_rt_version: &str,
    builder_sha256: &str,
    model: &ModelRuntime,
) -> String {
    let profile = &model.profile;
    let material = format!(
        "gpu_uuid={}\ndriver={}\ntensorrt={tensor_rt_version}\nruntime_abi={RIFE_RUNTIME_ABI}\nbuilder_sha256={builder_sha256}\nmodel={}\nmodel_sha256={}\nscale={}\nprecision={RIFE_PRECISION}\nalignment={}\nprofile={}x{}+{}x{}+{}x{}",
        gpu.uuid,
        gpu.driver,
        model.model.as_str(),
        model.model_sha256,
        model.model.scale(),
        model.model.shape_alignment(),
        profile.min_width,
        profile.min_height,
        profile.opt_width,
        profile.opt_height,
        profile.max_width,
        profile.max_height,
    );
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

fn validate_profile(
    model: InterpolationModel,
    profile: &EngineProfile,
) -> Result<(), InterpolationError> {
    let alignment = model.shape_alignment();
    let expected_opt_height = if alignment == 64 { 1_088 } else { 1_152 };
    let values = [
        profile.min_width,
        profile.min_height,
        profile.opt_width,
        profile.opt_height,
        profile.max_width,
        profile.max_height,
    ];
    if profile.min_width != alignment
        || profile.min_height != alignment
        || profile.opt_width != 1_920
        || profile.opt_height != expected_opt_height
        || profile.max_width != RIFE_PROFILE_MAX
        || profile.max_height != RIFE_PROFILE_MAX
        || values.iter().any(|value| value % alignment != 0)
        || profile.min_width > profile.opt_width
        || profile.min_height > profile.opt_height
        || profile.opt_width > profile.max_width
        || profile.opt_height > profile.max_height
    {
        return Err(InterpolationError::new(
            "frame_interpolation_manifest_invalid",
            format!(
                "{} has an invalid dynamic Engine profile",
                model.display_name()
            ),
        ));
    }
    Ok(())
}

fn validate_cached_engine(
    engine_path: &Path,
    metadata_path: &Path,
    expected_key: &str,
) -> Result<Option<EngineArtifact>, InterpolationError> {
    let engine_exists = engine_path.is_file();
    let metadata_exists = metadata_path.is_file();
    if !engine_exists && !metadata_exists {
        return Ok(None);
    }
    if !engine_exists || !metadata_exists {
        return Err(InterpolationError::new(
            "frame_interpolation_engine_corrupt",
            format!(
                "RIFE Engine cache is incomplete: engine={} metadata={}",
                engine_path.display(),
                metadata_path.display()
            ),
        ));
    }
    let bytes = std::fs::read(metadata_path).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_engine_corrupt",
            format!("{} could not be read: {error}", metadata_path.display()),
        )
    })?;
    let metadata: Value = serde_json::from_slice(&bytes).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_engine_corrupt",
            format!("{} is invalid JSON: {error}", metadata_path.display()),
        )
    })?;
    let key = metadata.get("engineKey").and_then(Value::as_str);
    let file = metadata.get("engineFile").and_then(Value::as_str);
    let expected_hash = metadata.get("sha256").and_then(Value::as_str);
    if metadata.get("schema").and_then(Value::as_u64) != Some(1)
        || key != Some(expected_key)
        || file != engine_path.file_name().and_then(|value| value.to_str())
        || expected_hash.is_none()
    {
        return Err(InterpolationError::new(
            "frame_interpolation_engine_corrupt",
            format!(
                "{} does not match its Engine cache key",
                metadata_path.display()
            ),
        ));
    }
    let actual_hash = hash_file(
        engine_path,
        "frame_interpolation_engine_missing",
        "frame_interpolation_engine_corrupt",
    )?;
    if !actual_hash.eq_ignore_ascii_case(expected_hash.unwrap_or_default()) {
        return Err(InterpolationError::new(
            "frame_interpolation_engine_corrupt",
            format!("{} hash does not match its metadata", engine_path.display()),
        ));
    }
    Ok(Some(EngineArtifact {
        key: expected_key.to_string(),
        path: engine_path.to_path_buf(),
    }))
}

fn cleanup_obsolete_engine_cache(
    cache_dir: &Path,
    active_keys: &[String],
) -> Result<(), InterpolationError> {
    let entries = std::fs::read_dir(cache_dir).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_engine_cache_unavailable",
            format!("{} could not be scanned: {error}", cache_dir.display()),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            InterpolationError::new(
                "frame_interpolation_engine_cache_unavailable",
                format!(
                    "{} contains an unreadable entry: {error}",
                    cache_dir.display()
                ),
            )
        })?;
        let file_type = entry.file_type().map_err(|error| {
            InterpolationError::new(
                "frame_interpolation_engine_cache_unavailable",
                format!("{} type could not be read: {error}", entry.path().display()),
            )
        })?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let managed = name.ends_with(".engine")
            || name.ends_with(".json")
            || name.ends_with(".runtime-cache")
            || name.contains(".building");
        let active = active_keys.iter().any(|key| {
            name == format!("{key}.engine")
                || name == format!("{key}.json")
                || name.starts_with(&format!("{key}.engine."))
        });
        if managed && !active {
            std::fs::remove_file(entry.path()).map_err(|error| {
                InterpolationError::new(
                    "frame_interpolation_engine_cache_unavailable",
                    format!("{} could not be removed: {error}", entry.path().display()),
                )
            })?;
        }
    }
    Ok(())
}

fn hash_file(
    path: &Path,
    missing_code: &'static str,
    corrupt_code: &'static str,
) -> Result<String, InterpolationError> {
    let mut file = File::open(path).map_err(|error| {
        InterpolationError::new(
            missing_code,
            format!("{} could not be opened: {error}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            InterpolationError::new(
                corrupt_code,
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

fn ensure_model_engine(
    runtime: &RuntimeComponents,
    model: &ModelRuntime,
) -> Result<EngineArtifact, InterpolationError> {
    if let Some(error) = &model.cached_engine_failure {
        return Err(error.clone());
    }
    if let Some(engine) = &model.engine {
        return Ok(engine.clone());
    }
    let _guard = ENGINE_BUILD_LOCK.lock().map_err(|_| {
        InterpolationError::new(
            "frame_interpolation_engine_build_failed",
            "The RIFE Engine build lock is poisoned",
        )
    })?;
    let engine_path = runtime
        .cache_dir
        .join(format!("{}.engine", model.engine_key));
    let metadata_path = runtime.cache_dir.join(format!("{}.json", model.engine_key));
    if let Some(engine) = validate_cached_engine(&engine_path, &metadata_path, &model.engine_key)? {
        return Ok(engine);
    }
    build_model_engine(runtime, model, &engine_path, &metadata_path)
}

fn build_model_engine(
    runtime: &RuntimeComponents,
    model: &ModelRuntime,
    engine_path: &Path,
    metadata_path: &Path,
) -> Result<EngineArtifact, InterpolationError> {
    let process_id = std::process::id();
    let temporary_engine = runtime.cache_dir.join(format!(
        "{}.{}.building.engine",
        model.engine_key, process_id
    ));
    let temporary_metadata = runtime
        .cache_dir
        .join(format!("{}.{}.building.json", model.engine_key, process_id));
    remove_temporary_file(&temporary_engine)?;
    remove_temporary_file(&temporary_metadata)?;

    let path_argument = |name: &str, path: &Path| {
        let mut value = OsString::from(name);
        value.push(path);
        value
    };
    let profile = &model.profile;
    let output = Command::new(&runtime.engine_builder)
        .current_dir(
            runtime
                .engine_builder
                .parent()
                .unwrap_or_else(|| Path::new(".")),
        )
        .arg(path_argument("--onnx=", &model.onnx_path))
        .arg(format!(
            "--minShapes=input:1x11x{}x{}",
            profile.min_height, profile.min_width
        ))
        .arg(format!(
            "--optShapes=input:1x11x{}x{}",
            profile.opt_height, profile.opt_width
        ))
        .arg(format!(
            "--maxShapes=input:1x11x{}x{}",
            profile.max_height, profile.max_width
        ))
        .arg(path_argument("--saveEngine=", &temporary_engine))
        .args(["--skipInference", "--useGpu"])
        .output()
        .map_err(|error| {
            InterpolationError::new(
                "frame_interpolation_engine_build_failed",
                format!(
                    "TensorRT-RTX builder could not start for {}: {error}",
                    model.model.display_name()
                ),
            )
        })?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&temporary_engine);
        return Err(InterpolationError::new(
            "frame_interpolation_engine_build_failed",
            format!(
                "TensorRT-RTX builder failed for {} with status {}: {}",
                model.model.display_name(),
                output.status,
                bounded_command_output(&output.stdout, &output.stderr)
            ),
        ));
    }
    if !temporary_engine.is_file() {
        return Err(InterpolationError::new(
            "frame_interpolation_engine_build_failed",
            format!(
                "TensorRT-RTX builder did not create an Engine for {}",
                model.model.display_name()
            ),
        ));
    }
    let engine_sha256 = hash_file(
        &temporary_engine,
        "frame_interpolation_engine_build_failed",
        "frame_interpolation_engine_corrupt",
    )?;
    let metadata = serde_json::json!({
        "schema": 1,
        "engineKey": model.engine_key,
        "engineFile": engine_path.file_name().and_then(|value| value.to_str()),
        "sha256": engine_sha256,
        "model": model.model.as_str(),
        "onnxSha256": model.model_sha256,
        "scale": model.model.scale(),
        "precision": RIFE_PRECISION,
        "shapeAlignment": model.model.shape_alignment(),
        "profile": {
            "minWidth": profile.min_width,
            "minHeight": profile.min_height,
            "optWidth": profile.opt_width,
            "optHeight": profile.opt_height,
            "maxWidth": profile.max_width,
            "maxHeight": profile.max_height,
        },
    });
    let metadata_bytes = serde_json::to_vec_pretty(&metadata).map_err(|error| {
        InterpolationError::new(
            "frame_interpolation_engine_build_failed",
            format!("RIFE Engine metadata could not be encoded: {error}"),
        )
    })?;
    std::fs::write(&temporary_metadata, metadata_bytes).map_err(|error| {
        let _ = std::fs::remove_file(&temporary_engine);
        InterpolationError::new(
            "frame_interpolation_engine_build_failed",
            format!(
                "{} could not be written: {error}",
                temporary_metadata.display()
            ),
        )
    })?;
    std::fs::rename(&temporary_engine, engine_path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary_engine);
        let _ = std::fs::remove_file(&temporary_metadata);
        InterpolationError::new(
            "frame_interpolation_engine_build_failed",
            format!("{} could not be committed: {error}", engine_path.display()),
        )
    })?;
    std::fs::rename(&temporary_metadata, metadata_path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary_metadata);
        InterpolationError::new(
            "frame_interpolation_engine_build_failed",
            format!(
                "{} could not be committed: {error}",
                metadata_path.display()
            ),
        )
    })?;
    Ok(EngineArtifact {
        key: model.engine_key.clone(),
        path: engine_path.to_path_buf(),
    })
}

fn remove_temporary_file(path: &Path) -> Result<(), InterpolationError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(InterpolationError::new(
            "frame_interpolation_engine_cache_unavailable",
            format!("{} could not be removed: {error}", path.display()),
        )),
    }
}

fn bounded_command_output(stdout: &[u8], stderr: &[u8]) -> String {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stderr),
        String::from_utf8_lossy(stdout)
    );
    text.trim().chars().take(4_096).collect()
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
    pub model: InterpolationModel,
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
    pub model_id: &'static str,
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
    let model = runtime
        .models
        .iter()
        .find(|model| model.model == request.model)
        .ok_or_else(|| {
            InterpolationError::new(
                "frame_interpolation_model_unavailable",
                format!("{} is not installed", request.model.display_name()),
            )
        })?;
    let engine = ensure_model_engine(runtime, model)?;
    let video_filter = format!(
        "nvofmemc=rife=yes:rife-model={}:rife-source-width={}:rife-source-height={}:rife-shape-alignment={}:rife-runtime-dll={}:rife-engine={}:rife-cudart={}",
        model.model.as_str(),
        request.width,
        request.height,
        model.model.shape_alignment(),
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
        model_id: model.model.as_str(),
        model: model.model.display_name().to_string(),
        model_sha256: model.model_sha256.clone(),
        engine_key: engine.key.clone(),
        engine_path: engine.path.clone(),
        scale: model.model.scale(),
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
    if width > RIFE_PROFILE_MAX
        || height > RIFE_PROFILE_MAX
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
    {
        return Err(InterpolationError::new(
            "frame_interpolation_dimensions_unsupported",
            format!(
                "RIFE interpolation requires even dimensions up to {RIFE_PROFILE_MAX}x{RIFE_PROFILE_MAX}, received {width}x{height}"
            ),
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
        let model_runtime = |model: InterpolationModel| {
            let alignment = model.shape_alignment();
            ModelRuntime {
                model,
                model_sha256: format!("{}-sha256", model.as_str()),
                onnx_path: PathBuf::from(format!("/runtime/{}.onnx", model.as_str())),
                profile: EngineProfile {
                    min_width: alignment,
                    min_height: alignment,
                    opt_width: 1_920,
                    opt_height: if alignment == 64 { 1_088 } else { 1_152 },
                    max_width: RIFE_PROFILE_MAX,
                    max_height: RIFE_PROFILE_MAX,
                },
                engine_key: format!("{}-dynamic", model.as_str()),
                engine: Some(EngineArtifact {
                    key: format!("{}-dynamic", model.as_str()),
                    path: PathBuf::from(format!("/runtime/{}.engine", model.as_str())),
                }),
                cached_engine_failure: None,
            }
        };
        CapabilityReport {
            ready: true,
            runtime_version: Some(format!("TensorRT-RTX {RIFE_TENSORRT_VERSION}")),
            model: Some("RIFE v4.26, RIFE v4.26 (scale=0.5), RIFE v4.25 Lite".to_string()),
            models: InterpolationModel::ALL
                .into_iter()
                .map(|model| ModelCapability {
                    id: model.as_str(),
                    name: model.display_name(),
                    engine_count: 1,
                })
                .collect(),
            engine_count: 3,
            backend: Some(RIFE_BACKEND),
            filter: Some(RIFE_FILTER),
            runtime: Some(RuntimeComponents {
                runtime_dll: PathBuf::from("/runtime/rife_runtime.dll"),
                cuda_runtime_dll: PathBuf::from("/runtime/cudart64_12.dll"),
                engine_builder: PathBuf::from("/runtime/tensorrt_rtx.exe"),
                tensor_rt_version: RIFE_TENSORRT_VERSION.to_string(),
                cache_dir: PathBuf::from("/cache/frame-interpolation/engine-cache"),
                models: InterpolationModel::ALL
                    .into_iter()
                    .map(model_runtime)
                    .collect(),
            }),
            ..CapabilityReport::default()
        }
    }

    fn request(mode: InterpolationMode, width: u32, height: u32, source_fps: f64) -> PlanRequest {
        PlanRequest {
            mode,
            model: InterpolationModel::RifeV426,
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
    fn models_round_trip() {
        for model in InterpolationModel::ALL {
            assert_eq!(InterpolationModel::parse(model.as_str()), Some(model));
            assert!(!model.display_name().is_empty());
        }
        assert_eq!(InterpolationModel::parse("rife-auto"), None);
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
            assert!(plan.video_filter.contains("rife-model=rife-v4.26"));
            assert!(plan.video_filter.contains("rife-runtime-dll="));
            assert!(plan.video_filter.contains("rife-engine="));
            assert!(plan.video_filter.contains("rife-cudart="));
            assert_eq!(plan.model_id, "rife-v4.26");
            assert_eq!(plan.model, "RIFE v4.26");
            assert_eq!(plan.engine_key, "rife-v4.26-dynamic");
            assert_eq!(plan.scale, "1.0");
            assert_eq!(plan.precision, "fp16");
            assert!(plan.video_filter.contains("rife-shape-alignment=64"));
        }
    }

    #[test]
    fn plan_uses_the_explicit_model_without_fallback() {
        let mut input = request(InterpolationMode::X2, 3840, 2160, 24.0);
        input.model = InterpolationModel::RifeV425Lite;
        let plan = build_plan(&ready_report(), input).expect("Lite model plan");
        assert_eq!(plan.model_id, "rife-v4.25-lite");
        assert_eq!(plan.model, "RIFE v4.25 Lite");
        assert_eq!(plan.engine_key, "rife-v4.25-lite-dynamic");
        assert!(plan.video_filter.contains("rife-model=rife-v4.25-lite"));
        assert!(plan.video_filter.contains("rife-shape-alignment=128"));

        let mut report = ready_report();
        report
            .runtime
            .as_mut()
            .expect("runtime")
            .models
            .retain(|model| model.model == InterpolationModel::RifeV426);
        let mut missing = request(InterpolationMode::X2, 3840, 2160, 24.0);
        missing.model = InterpolationModel::RifeV425Lite;
        assert_eq!(
            build_plan(&report, missing)
                .expect_err("model selection must not fall back")
                .code,
            "frame_interpolation_model_unavailable"
        );
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
    fn dynamic_profile_accepts_arbitrary_even_dimensions() {
        assert!(validate_dimensions(3840, 2160).is_ok());
        assert!(validate_dimensions(7680, 4320).is_ok());
        assert!(validate_dimensions(1918, 1078).is_ok());
        assert!(
            build_plan(
                &ready_report(),
                request(InterpolationMode::X2, 1920, 800, 24.0),
            )
            .is_ok()
        );
        assert_eq!(
            validate_dimensions(1919, 1079)
                .expect_err("P010 dimensions must be even")
                .code,
            "frame_interpolation_dimensions_unsupported"
        );
    }

    #[test]
    fn scale05_is_an_explicit_v426_model_without_fallback() {
        let mut input = request(InterpolationMode::X2, 2560, 1080, 24.0);
        input.model = InterpolationModel::RifeV426Scale05;
        let plan = build_plan(&ready_report(), input).expect("scale=0.5 plan");
        assert_eq!(plan.model_id, "rife-v4.26-scale0.5");
        assert_eq!(plan.scale, "0.5");
        assert_eq!(plan.engine_key, "rife-v4.26-scale0.5-dynamic");
        assert!(plan.video_filter.contains("rife-shape-alignment=128"));
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
