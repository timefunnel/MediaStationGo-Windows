# RTX frame interpolation dependency record

The production playback path uses the native `NVOFA + D3D11` mpv filter. The
project's independent provenance is not treated as a licensing blocker, while
third-party component notices and terms still apply to distributed artifacts.

| Production component | Baseline | Distribution note |
| --- | --- | --- |
| NVIDIA Optical Flow API | API 5.0 headers | SDK headers are sourced from NVIDIA; `nvofapi64.dll` is supplied by the installed NVIDIA driver and is not bundled |
| Direct3D 11 / D3DCompiler | Windows system components | `d3dcompiler_47.dll` is probed explicitly at startup |
| mpv native filter | Local `vf_nvofa` implementation | Built into the project's pinned mpv source build |

The native feature currently accepts NVIDIA RTX GPUs, 1080p/1440p/4K input,
SDR or HDR10, and 59.94/60 FPS output. HLG remains disabled pending validation;
HDR10+ and Dolby Vision are rejected because dynamic metadata is not preserved.
There is no silent fallback to another interpolation implementation.

## Experimental RIFE baseline

The VapourSynth/vs-mlrt/RIFE path remains an experimental PoC and is not the
default playback backend.

| Experimental component | Pinned baseline | License / distribution status |
| --- | --- | --- |
| VapourSynth | R65 (`mingw-w64-clang-x86_64-vapoursynth` 65-8) | LGPL-2.1-or-later; MSYS2 package metadata verified locally |
| vs-mlrt scripts and VSTRT-RTX plugin | v15.16 | GPL-3.0; retain source and notices for a distribution that includes it |
| TensorRT-RTX | 1.4.0.76, CUDA 13.2 build | NVIDIA proprietary SDK |
| RIFE model | v4.25 Lite, implementation 1 ONNX | Supplied by the vs-mlrt external-models release |
| onnxconverter-common | 1.16.0 | Apache-2.0 |
| CUDA Runtime | Caller-supplied `cudart64_<version>.dll` | Required only by the native D3D11/CUDA interoperability probe; copied into the isolated runtime and recorded by filename and SHA-256 |

Development artifact hashes:

- `VSTRT-RTX-Windows-x64.v15.16.7z`: `d2d311b09635d6681285aa4eb30030b953ed243b49c6d842c490d32c338ca303`
- `scripts.v15.16.7z`: `d07dae0a00cb8dbf4f00358f640f630ff5d933de44d27050e7acce4f31cc3560`
- `rife_v4.25_lite.7z`: `7d53e29fff5e67345b19f4ce97dfd1e34b490eedc752df2236904fda1a13842c`
- `TensorRT-RTX-1.4.0.76-Windows-amd64-cuda-13.2-Release-external.zip`: `0a050b10158bbe286c90b55b23dffbd3d5096c626b2ee45eccf51322795a3c29`
- `onnxconverter_common-1.16.0-py2.py3-none-any.whl`: `df39ee96f17fff119dff10dd245467651b60b9e8a96020eb93402239794852f7`

The experimental preparation script requires an explicit
`-AcceptNvidiaLicense` switch and a caller-provided TensorRT-RTX archive. It
never downloads the NVIDIA SDK itself. The generated runtime stays under
ignored `third_party/`.

## Known baseline constraint

vs-mlrt v15.16 explicitly rejects `scale != 1.0` for RIFE v4.25 Lite in
`RIFEMerge`. This constraint does not apply to the production NVOFA backend.

## Native D3D11/TensorRT validation

The VapourSynth path is not suitable for full-resolution 4K P010 playback. On
the RTX 5070 Ti reference machine, implementation 1 reached 28.63 output FPS
for strict 24 to 48 interpolation because CPU color conversion dominated the
filter graph. Implementation 2 was also rejected as a 4K baseline: its FP32
engine reached only 26.89 pair FPS with a 37.90 ms p95 before conversion.

`run_rife_d3d11_trt_probe.ps1` validates the replacement data path without
altering the player: two P010 D3D11 textures are converted into the exact
11-channel FP16 RIFE tensor by a compute shader, shared with CUDA, inferred by
TensorRT-RTX, converted back to P010, synchronized, and read back once for
packing validation. No frame crosses system memory during the timed loop.

Reference results for RIFE v4.25 Lite implementation 1, strict x2:

| Source | Iterations | Throughput | p95 | P010 validation |
| --- | ---: | ---: | ---: | --- |
| 1920x1080 | 300 | 157.54 pair FPS | 6.62 ms | 877 luma codes; 1,553,080 non-8-bit luma samples |
| 2560x1440 | 300 | 87.45 pair FPS | 11.82 ms | 877 luma codes; 2,764,057 non-8-bit luma samples |
| 3840x2160 | 600 | 41.87 pair FPS | 24.33 ms | 877 luma codes; 6,216,617 non-8-bit luma samples |

These measurements prove the conversion, interop, inference, and P010 packing
budget only. They do not yet prove scene-cut quality, mpv scheduling, decoded
frame ownership, HDR metadata propagation, subtitles, seek/reset behavior, or
long-form playback stability. The native path must remain outside production
playback until those items pass explicit validation.
