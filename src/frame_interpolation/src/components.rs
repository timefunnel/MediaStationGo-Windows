//! Identities for optional native video components. RIFE keeps its existing paths.

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ComponentKind {
    Rife,
    RtxVideo,
    DlssNr,
    NvidiaVfg,
}

impl ComponentKind {
    pub const ALL: [Self; 4] = [Self::Rife, Self::RtxVideo, Self::DlssNr, Self::NvidiaVfg];

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == value)
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Rife => "rife",
            Self::RtxVideo => "rtx-video",
            Self::DlssNr => "dlssnr",
            Self::NvidiaVfg => "nvidia-vfg",
        }
    }

    pub const fn cache_name(self) -> &'static str {
        match self {
            Self::Rife => "frame-interpolation",
            _ => self.id(),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Rife => "RIFE 插帧",
            Self::RtxVideo => "RTX Video 超分",
            Self::DlssNr => "DLSSNR 画面增强（实验）",
            Self::NvidiaVfg => "NVIDIA VFG 插帧",
        }
    }

    pub const fn runtime_version(self) -> &'static str {
        match self {
            Self::Rife => "1.4.0.76",
            Self::RtxVideo => "1.6.0.0",
            Self::DlssNr => "310.8.0.0",
            Self::NvidiaVfg => "1.3.0",
        }
    }

    pub const fn release_tag(self) -> &'static str {
        match self {
            Self::Rife => super::component::RELEASE_TAG,
            Self::RtxVideo => "rtx-video-component-x64-v1",
            Self::DlssNr => "dlssnr-component-x64-v1",
            Self::NvidiaVfg => "nvidia-vfg-component-x64-v1",
        }
    }

    pub const fn asset_name(self) -> &'static str {
        match self {
            Self::Rife => super::component::ASSET_NAME,
            Self::RtxVideo => "MediaStationGo-rtx-video-x64-v1.zip",
            Self::DlssNr => "MediaStationGo-dlssnr-x64-v1.zip",
            Self::NvidiaVfg => "MediaStationGo-nvidia-vfg-x64-v1.zip",
        }
    }

    pub(crate) const fn files(self) -> &'static [&'static str] {
        match self {
            Self::Rife => super::component::FILES,
            Self::RtxVideo => &["nvngx_vsr.dll", "licenses/NVIDIA-LICENSE.pdf"],
            Self::DlssNr => &["nvngx_dlssnr.dll", "licenses/NVIDIA-LICENSE.txt"],
            Self::NvidiaVfg => &[
                "NVVideoEffects.dll",
                "NVCVImage.dll",
                "nvVFXVideoFrameGeneration.dll",
                "nvngxruntime.dll",
                "cudart64_12.dll",
                "licenses/NVIDIA-Software-License-Agreement.pdf",
                "licenses/NVIDIA-Open-Model-License.pdf",
                "licenses/NVIDIA-AI-Product-Terms.pdf",
                "licenses/THIRD_PARTY.md",
                "licenses/ThirdPartyLicenses_VFX_SDK.txt",
            ],
        }
    }
}
