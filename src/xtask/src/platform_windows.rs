use crate::{BuildArgs, cef, fs as xfs, mpv};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn stage_cef(out: &Path, cef: &cef::Cef) -> Result<()> {
    if cef.link_external {
        return Ok(());
    }
    xfs::copy_glob(&cef.dir, out, &["*.dll", "*.bin", "*.json"])?;
    xfs::copy_glob(&cef.dir, out, &["*.pak", "*.dat"])?;
    xfs::copy_dir_recursive(&cef.dir.join("locales"), &out.join("locales"))?;
    Ok(())
}

pub fn stage_mpv(out: &Path, mpv_info: &mpv::Mpv, used_external: bool, _bin: &Path) -> Result<()> {
    remove_bundled_interpolation(out)?;
    if !used_external {
        let runtime = mpv::runtime_library_name();
        xfs::copy_file(&mpv_info.build_dir.join(runtime), &out.join(runtime))?;
    } else {
        let lib_dir = mpv_info.build_dir.join("lib");
        for entry in std::fs::read_dir(&lib_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().ends_with(".dll")
                && !COMPONENT_BINARIES.contains(&name.to_string_lossy().as_ref())
            {
                std::fs::copy(entry.path(), out.join(&name))?;
            }
        }
    }
    Ok(())
}

pub fn install(build_dir: &Path, prefix: &Path, args: &BuildArgs) -> Result<PathBuf> {
    remove_bundled_interpolation(prefix)?;
    xfs::copy_executable(
        &build_dir.join("jellium-desktop.exe"),
        &prefix.join("jellium-desktop.exe"),
    )?;
    xfs::copy_executable(
        &build_dir.join("mediastation-portable-updater.exe"),
        &prefix.join("mediastation-portable-updater.exe"),
    )?;
    if args.cef_path.is_none() {
        let cef = cef::discover(&args.external_cef)?;
        xfs::copy_glob(&cef.dir, prefix, &["*.dll", "*.bin", "*.json"])?;
        xfs::copy_glob(&cef.dir, prefix, &["*.pak", "*.dat"])?;
        xfs::copy_dir_recursive(&cef.dir.join("locales"), &prefix.join("locales"))?;
    }
    if let Some(dir) = &args.external_mpv {
        for entry in std::fs::read_dir(dir.join("lib"))? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().ends_with(".dll")
                && !COMPONENT_BINARIES.contains(&name.to_string_lossy().as_ref())
            {
                xfs::copy_file(&entry.path(), &prefix.join(name))?;
            }
        }
    }
    Ok(prefix.to_path_buf())
}

const COMPONENT_BINARIES: &[&str] = &[
    "rife_runtime.dll",
    "cudart64_12.dll",
    "tensorrt_rtx_1_4.dll",
    "tensorrt_onnxparser_rtx_1_4.dll",
    "rife_engine_builder.exe",
    "tensorrt_rtx.exe",
];

fn remove_bundled_interpolation(directory: &Path) -> Result<()> {
    // Only obsolete, formerly managed payloads. Preserve unknown files/caches.
    for name in COMPONENT_BINARIES.iter().copied().chain([
        "frame-interpolation/runtime-manifest.json",
        "frame-interpolation/models/rife_v4.26_fp16_io.onnx",
        "frame-interpolation/models/rife_v4.26_scale0.5.onnx",
        "frame-interpolation/models/rife_v4.25_lite_fp16_io.onnx",
    ]) {
        let path = directory.join(name);
        for ancestor in path
            .ancestors()
            .take_while(|path| path.starts_with(directory))
        {
            match std::fs::symlink_metadata(ancestor) {
                Ok(metadata) => {
                    use std::os::windows::fs::MetadataExt as _;
                    if metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
                    {
                        anyhow::bail!(
                            "refuse obsolete component cleanup through reparse path: {}",
                            ancestor.display()
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("inspect cleanup parent {}", ancestor.display()));
                }
            }
        }
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                std::fs::remove_file(&path).with_context(|| {
                    format!("remove obsolete bundled component {}", path.display())
                })?
            }
            Ok(_) => anyhow::bail!(
                "obsolete bundled component is not a regular file: {}",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect obsolete component {}", path.display()));
            }
        }
    }
    for name in ["frame-interpolation/models", "frame-interpolation"] {
        let path = directory.join(name);
        match std::fs::read_dir(&path) {
            Ok(mut entries) => match entries.next() {
                None => std::fs::remove_dir(&path).with_context(|| {
                    format!("remove empty former component directory {}", path.display())
                })?,
                Some(entry) => {
                    entry?;
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect former component directory {}", path.display())
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_removes_only_known_bundled_component_files() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path();
        let legacy = directory.join("tensorrt_rtx.exe");
        let owned = directory.join("libmpv-2.dll");
        let models = directory.join("frame-interpolation/models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("rife_v4.26_scale0.5.onnx"), b"old model").unwrap();
        std::fs::write(models.join("user-note.txt"), b"preserve").unwrap();
        std::fs::write(&legacy, b"legacy").expect("create legacy tool");
        std::fs::write(&owned, b"owned").expect("create application helper");
        remove_bundled_interpolation(directory).expect("remove obsolete component");
        assert!(!legacy.exists());
        assert!(!models.join("rife_v4.26_scale0.5.onnx").exists());
        assert_eq!(
            std::fs::read(models.join("user-note.txt")).unwrap(),
            b"preserve"
        );
        assert_eq!(std::fs::read(&owned).expect("read helper"), b"owned");
        remove_bundled_interpolation(directory).expect("idempotent cleanup");
        temp.close().unwrap();
    }
}
