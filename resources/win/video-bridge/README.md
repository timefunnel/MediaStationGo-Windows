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
| Build date | 2026-10-25 |
| SDK | `third_party/video-enhancement-deps/RTX-Video-SDK-1.1.0` |
| Sources | `dev/windows/mpv/video_enhancement.cpp`, `hdr_pipeline.h`, `dovi_pipeline.h`, `video_enhancement.h`, `nr_adapter.cpp` |
| Video bridge source | Git blob `7682cb73c1b902a3d93fffb17c2cbe7628139fc2` (`dev/windows/mpv/video_enhancement.cpp`) |
| HDR/Dolby mapping sources | Git blobs `10cf6cf3e32799b317f5b0715171a0727f2a0f65` / `7bb2bcd31f26e6d007a49c3799c2674e552e6ed5` |
| Video bridge ABI | 7; per-frame RPU mapping and FEL NLQ reconstruction, paired with the updated mpv `nvvideo` filter |
| NR adapter source | `364e5ca` (unchanged since) |
| `video_enhancement.dll` SHA-256 | `3ef769cc4c6a5c1fa96c80a9b5ebfde73341b3d1060d800927b65068ca98a6f2` |
| `nvngx.dll` SHA-256 | `fe7fe1093b1ae899d56e37935a24823926faad868089e99b38746288b396b6f0` |

`NVIDIA_RTX_Video_SDK_License.pdf` is the SDK notice shipped in the release
`licenses/` directory; its SHA-256 is
`5addfcf7c78891a12f1dcd6b5f86c2512351ab71b3872f446ba10e1b6da54464`.

## Rebuild

With the NVIDIA RTX Video SDK placed under `third_party/video-enhancement-deps`:

```powershell
.\dev\windows\build_video_enhancement.ps1
Copy-Item build\video-enhancement\video_enhancement.dll,build\video-enhancement\nvngx.dll resources\win\video-bridge\ -Force
```

Recompute the hashes above and commit the refreshed binaries together with the
source change. CI fails the package verification when either DLL is missing.
