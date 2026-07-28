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
