use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

const NVOFA_REQUIRED_API: u32 = 0x50;
const NVOFA_BACKEND: &str = "NVOFA D3D11";
const NVOFA_FILTER: &str = "vf_nvofa";
const DEFAULT_SCENE_THRESHOLD: f64 = 0.35;

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

    pub const fn supported_by_native_backend(self) -> bool {
        matches!(self, Self::Off | Self::Auto | Self::Fps60)
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
    pub optical_flow_api: Option<String>,
    pub backend: Option<&'static str>,
    pub filter: Option<&'static str>,
    pub failure: Option<InterpolationError>,
}

#[derive(Clone, Debug)]
struct GpuInfo {
    name: String,
    uuid: String,
    driver: String,
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
        Ok((gpu, api_version)) => CapabilityReport {
            ready: true,
            gpu_name: Some(gpu.name),
            gpu_uuid: Some(gpu.uuid),
            driver_version: Some(gpu.driver),
            optical_flow_api: Some(format_api_version(api_version)),
            backend: Some(NVOFA_BACKEND),
            filter: Some(NVOFA_FILTER),
            failure: None,
        },
        Err(error) => CapabilityReport {
            failure: Some(error),
            ..CapabilityReport::default()
        },
    }
}

fn probe_capability() -> Result<(GpuInfo, u32), InterpolationError> {
    if !cfg!(target_os = "windows") {
        return Err(InterpolationError::new(
            "frame_interpolation_platform_unsupported",
            "NVOFA frame interpolation is supported only on Windows",
        ));
    }
    let gpu = probe_gpu()?;
    let api_version = probe_nvofa_api()?;
    if api_version < NVOFA_REQUIRED_API {
        return Err(InterpolationError::new(
            "frame_interpolation_nvofa_api_unsupported",
            format!(
                "NVOFA API {} is below required 5.0",
                format_api_version(api_version)
            ),
        ));
    }
    probe_d3d_compiler()?;
    Ok((gpu, api_version))
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

#[cfg(target_os = "windows")]
fn probe_nvofa_api() -> Result<u32, InterpolationError> {
    use std::ffi::{CString, OsStr};
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    type GetMaxSupportedApiVersion = unsafe extern "system" fn(*mut u32) -> u32;
    let library_name = OsStr::new("nvofapi64.dll")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let module = unsafe { LoadLibraryW(library_name.as_ptr()) };
    if module.is_null() {
        return Err(InterpolationError::new(
            "frame_interpolation_nvofa_library_unavailable",
            format!(
                "nvofapi64.dll could not be loaded: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    let symbol =
        CString::new("NvOFGetMaxSupportedApiVersion").expect("static NVOFA symbol has no NUL");
    let address = unsafe { GetProcAddress(module, symbol.as_ptr().cast()) };
    let result = if let Some(address) = address {
        let function: GetMaxSupportedApiVersion = unsafe { std::mem::transmute(address) };
        let mut version = 0_u32;
        let status = unsafe { function(&mut version) };
        if status == 0 {
            Ok(version)
        } else {
            Err(InterpolationError::new(
                "frame_interpolation_nvofa_probe_failed",
                format!("NvOFGetMaxSupportedApiVersion returned status {status}"),
            ))
        }
    } else {
        Err(InterpolationError::new(
            "frame_interpolation_nvofa_api_missing",
            "NvOFGetMaxSupportedApiVersion is missing from nvofapi64.dll",
        ))
    };
    unsafe { FreeLibrary(module) };
    result
}

#[cfg(not(target_os = "windows"))]
fn probe_nvofa_api() -> Result<u32, InterpolationError> {
    Err(InterpolationError::new(
        "frame_interpolation_platform_unsupported",
        "NVOFA frame interpolation is supported only on Windows",
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
        "NVOFA frame interpolation is supported only on Windows",
    ))
}

fn format_api_version(version: u32) -> String {
    format!("{}.{}", version >> 4, version & 0x0f)
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
    pub target_fps: f64,
    pub target_fps_num: u32,
    pub target_fps_den: u32,
    pub source_fps_num: u32,
    pub source_fps_den: u32,
    pub video_filter: String,
    pub hwdec: &'static str,
    pub backend: &'static str,
    pub optical_flow_api: String,
    pub scene_threshold: f64,
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
            "The NVOFA capability probe did not report a usable runtime",
        ));
    }
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
    let video_filter =
        format!("nvofa=target-fps={target_fps:.6}:scene-threshold={DEFAULT_SCENE_THRESHOLD:.2}");
    Ok(InterpolationPlan {
        mode: request.mode,
        target_fps,
        target_fps_num,
        target_fps_den,
        source_fps_num,
        source_fps_den,
        video_filter,
        hwdec: "d3d11va",
        backend: NVOFA_BACKEND,
        optical_flow_api: report
            .optical_flow_api
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        scene_threshold: DEFAULT_SCENE_THRESHOLD,
    })
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
            format!("NVOFA first release supports at most 3840x2160, received {width}x{height}"),
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
        InterpolationMode::Fps90 | InterpolationMode::Fps120 => {
            return Err(InterpolationError::new(
                "frame_interpolation_target_not_supported",
                format!(
                    "NVOFA first release supports only 60 FPS output, requested {} FPS",
                    mode.as_str()
                ),
            ));
        }
        InterpolationMode::Off => unreachable!("off mode exits before target selection"),
        InterpolationMode::Auto | InterpolationMode::Fps60 => {}
    }
    if display_fps < 58.0 {
        return Err(InterpolationError::new(
            "frame_interpolation_display_refresh_unsupported",
            format!("Display refresh {display_fps:.3} Hz is below 60 Hz"),
        ));
    }
    let fractional_source = source_den == 1_001 && matches!(source_num, 24_000 | 30_000 | 48_000);
    let target = if fractional_source && display_fps < 60.5 {
        (60_000, 1_001)
    } else {
        (60, 1)
    };
    let target_value = f64::from(target.0) / f64::from(target.1);
    if display_fps + 1.0 < target_value {
        return Err(InterpolationError::new(
            "frame_interpolation_display_refresh_insufficient",
            format!("Target {target_value:.3} FPS exceeds display refresh {display_fps:.3} Hz"),
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
        CapabilityReport {
            ready: true,
            optical_flow_api: Some("5.0".to_string()),
            backend: Some(NVOFA_BACKEND),
            filter: Some(NVOFA_FILTER),
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
            InterpolationMode::Fps60,
            InterpolationMode::Fps90,
            InterpolationMode::Fps120,
        ] {
            assert_eq!(InterpolationMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(InterpolationMode::parse("59"), None);
        assert!(InterpolationMode::Fps60.supported_by_native_backend());
        assert!(!InterpolationMode::Fps90.supported_by_native_backend());
        assert!(!InterpolationMode::Fps120.supported_by_native_backend());
    }

    #[test]
    fn auto_and_explicit_60_use_native_filter() {
        for mode in [InterpolationMode::Auto, InterpolationMode::Fps60] {
            let plan = build_plan(&ready_report(), request(mode, 3840, 2160, 24.0))
                .expect("4K 24 to 60 plan");
            assert_eq!(plan.target_fps, 60.0);
            assert_eq!(plan.hwdec, "d3d11va");
            assert_eq!(plan.backend, NVOFA_BACKEND);
            assert_eq!(
                plan.video_filter,
                "nvofa=target-fps=60.000000:scene-threshold=0.35"
            );
        }
    }

    #[test]
    fn fractional_sources_target_5994_on_5994_display() {
        let mut input = request(InterpolationMode::Fps60, 1920, 1080, 23.976);
        input.display_fps = 59.94;
        let plan = build_plan(&ready_report(), input).expect("fractional plan");
        assert_eq!((plan.target_fps_num, plan.target_fps_den), (60_000, 1_001));
    }

    #[test]
    fn first_release_rejects_90_and_120() {
        for mode in [InterpolationMode::Fps90, InterpolationMode::Fps120] {
            let error = build_plan(&ready_report(), request(mode, 1920, 1080, 24.0))
                .expect_err("unsupported target must fail");
            assert_eq!(error.code, "frame_interpolation_target_not_supported");
        }
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
    fn dynamic_metadata_formats_remain_explicitly_rejected() {
        assert!(validate_dynamic_range(Some("SDR")).is_ok());
        assert!(validate_dynamic_range(Some("HDR10")).is_ok());
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
    fn hdr10_requires_pq_transfer_metadata() {
        assert!(validate_transfer(Some("HDR10"), Some("smpte2084")).is_ok());
        assert_eq!(
            validate_transfer(Some("HDR10"), Some("bt1886"))
                .expect_err("invalid HDR10 transfer must fail")
                .code,
            "frame_interpolation_hdr10_transfer_invalid"
        );
    }

    #[test]
    fn missing_server_color_metadata_is_deferred_to_native_frames() {
        let mut input = request(InterpolationMode::Fps60, 1920, 1080, 24.0);
        input.dynamic_range = None;
        input.color_space = None;
        input.color_transfer = None;
        input.color_range = None;
        let plan =
            build_plan(&ready_report(), input).expect("native filter validates decoded frames");
        assert_eq!(plan.hwdec, "d3d11va");
    }

    #[test]
    fn missing_hdr10_transfer_is_deferred_to_native_frames() {
        let mut input = request(InterpolationMode::Fps60, 3840, 2160, 24.0);
        input.dynamic_range = Some("HDR10".to_string());
        input.color_space = Some("bt2020nc".to_string());
        input.color_transfer = None;
        let plan =
            build_plan(&ready_report(), input).expect("native filter validates decoded frames");
        assert_eq!(plan.hwdec, "d3d11va");
    }

    #[test]
    fn known_fractional_rates_are_exact() {
        assert_eq!(rational_frame_rate(23.976), Ok((24_000, 1_001)));
        assert_eq!(rational_frame_rate(59.94), Ok((60_000, 1_001)));
        assert_eq!(rational_frame_rate(24.0), Ok((24, 1)));
    }
}
