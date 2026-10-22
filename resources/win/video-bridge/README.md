# Native video bridge (Windows x64)

The public Windows package must contain these two project-built DLLs next to
`jellium-desktop.exe`:

- `video_enhancement.dll` — D3D11/NGX video enhancement bridge.
- `nvngx.dll` — DLSSNR adapter (built from `dev/windows/mpv/nr_adapter.cpp`).

They are not NVIDIA redistributables; they are compiled by this project from the
sources below against the NVIDIA RTX Video SDK. The public GitHub Actions runner
does not have that SDK, so the checked-in binaries are staged by `cargo xtask`
instead of being rebuilt in CI.

## Provenance

| Item | Value |
| --- | --- |
| Build date | 2026-10-22 |
| SDK | `third_party/video-enhancement-deps/RTX-Video-SDK-1.1.0` |
| Sources | `dev/windows/mpv/video_enhancement.cpp`, `dev/windows/mpv/nr_adapter.cpp` |
| Source commit | `364e5ca` (unchanged since) |
| `video_enhancement.dll` SHA-256 | `11050f90014de9ba477d2c9678f7b1de5138465dbe4a90501dabdc3e822330d7` |
| `nvngx.dll` SHA-256 | `817b413833e96994e7152b0277ab63344ae4a7fc73453c3d04180cf0b8291673` |

## Rebuild

With the NVIDIA RTX Video SDK placed under `third_party/video-enhancement-deps`:

```powershell
.\dev\windows\build_video_enhancement.ps1
Copy-Item build\video-enhancement\video_enhancement.dll,build\video-enhancement\nvngx.dll resources\win\video-bridge\ -Force
```

Recompute the hashes above and commit the refreshed binaries together with the
source change. CI fails the package verification when either DLL is missing.
