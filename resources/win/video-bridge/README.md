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
| Video bridge source | Git blob `4af3bb9024abe32cdc278c7c4a5c116d9c96d845` (`dev/windows/mpv/video_enhancement.cpp`) |
| NR adapter source | `364e5ca` (unchanged since) |
| `video_enhancement.dll` SHA-256 | `97ffb5fe4d9d8a61ddc28cb0cfe411234fc6bc0661bee54a9b4bfdec41a9808a` |
| `nvngx.dll` SHA-256 | `817b413833e96994e7152b0277ab63344ae4a7fc73453c3d04180cf0b8291673` |

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
