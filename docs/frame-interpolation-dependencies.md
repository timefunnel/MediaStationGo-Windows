# RTX RIFE frame interpolation dependency record

The active playback path uses a native `D3D11 P010 -> TensorRT-RTX RIFE ->
D3D11 P010` bridge inside the pinned mpv build. No decoded or synthesized frame
crosses system memory. The project is independently developed, while
third-party component notices and terms still apply to distributed artifacts.

| Component | Pinned baseline | License / distribution status |
| --- | --- | --- |
| VapourSynth | R65 (`mingw-w64-clang-x86_64-vapoursynth` 65-8) | LGPL-2.1-or-later; MSYS2 package metadata verified locally |
| vs-mlrt scripts and VSTRT-RTX plugin | v15.16 | Used to prepare and validate engines; GPL-3.0 terms apply when these components are distributed |
| TensorRT-RTX | 1.4.0.76, CUDA 13.2 build | NVIDIA proprietary SDK |
| RIFE model | v4.26, implementation 1 ONNX | Supplied by the vs-mlrt external-models release |
| onnxconverter-common | 1.16.0 | Apache-2.0 |
| CUDA Runtime | `cudart64_12.dll` from the prepared runtime | Required by the native D3D11/CUDA interoperability bridge |
| Direct3D 11 / D3DCompiler | Windows system components | `d3dcompiler_47.dll` is probed explicitly at startup |
| mpv native filter | Local `vf_nvofmemc` RIFE mode | Built into the project's pinned mpv source build |

The playback runtime does not load Python, VapourSynth scripts, or vs-mlrt.
Those tools remain build-time inputs for producing the fixed-shape TensorRT
engines.

Development artifact hashes:

- `VSTRT-RTX-Windows-x64.v15.16.7z`: `d2d311b09635d6681285aa4eb30030b953ed243b49c6d842c490d32c338ca303`
- `scripts.v15.16.7z`: `d07dae0a00cb8dbf4f00358f640f630ff5d933de44d27050e7acce4f31cc3560`
- `rife_v4.26.7z`: `dfdabd84a2a3db773f87604b8cc255e94a6a72f13550d910ccd3b4ee2606cd4f`
- `rife_v4.26.onnx`: `af8392796b0ed769b8fcaeee0fbf5feee9c647d11a2575e50b620786f2536114`
- `TensorRT-RTX-1.4.0.76-Windows-amd64-cuda-13.2-Release-external.zip`: `0a050b10158bbe286c90b55b23dffbd3d5096c626b2ee45eccf51322795a3c29`
- `onnxconverter_common-1.16.0-py2.py3-none-any.whl`: `df39ee96f17fff119dff10dd245467651b60b9e8a96020eb93402239794852f7`

The preparation script requires an explicit
`-AcceptNvidiaLicense` switch and a caller-provided TensorRT-RTX archive. It
never downloads the NVIDIA SDK itself. The generated runtime stays under
ignored `third_party/`.

## Runtime contract

The current implementation is intentionally strict:

- NVIDIA RTX on Windows only.
- Strict x2 interpolation for 20-30 FPS sources.
- Exact engine shapes: 1920x1080, 2304x1296, 2560x1440, and 3840x2160.
- Full-resolution `scale=1.0`, FP16 inference, and `hwdec=d3d11va`.
- SDR and HDR10 are accepted. HLG is disabled pending validation. HDR10+ and
  Dolby Vision are rejected because dynamic metadata is not preserved.
- Input and output stay at P010. Original frames are copied without RGB
  conversion; only generated midpoint frames pass through the RIFE RGB tensor.
- Runtime, engine, CUDA, format, or inference failures are fatal for that
  playback request. There is no silent fallback to NVOFA, passthrough, frame
  blending, or resolution reduction.

Each engine is keyed by GPU UUID, NVIDIA driver, TensorRT version, model hash,
resolution, scale, and precision. The application verifies the manifest,
engine key, and engine SHA-256 at startup. The currently staged engines are
therefore valid only for the GPU and driver that built them; another machine or
driver requires an explicit engine rebuild.

vs-mlrt v15.16 rejects `scale != 1.0` for RIFE v4.26 in `RIFEMerge`.
The native runtime keeps the same full-resolution constraint and does not tile.

## Native mpv validation

The native D3D11 runtime has been exercised on the RTX 5070 Ti reference
machine. Timings below include P010 conversion, D3D11/CUDA sharing, inference,
output conversion, and synchronization after the first engine warm-up outlier.

| Source | Midpoints | Result | Inference p95 | Decoded / filter output |
| --- | ---: | --- | ---: | --- |
| 1920x1080 SDR real film | 58 | 58 inferred, 0 failed | 9.08 ms | 3 hard cuts copied from F0 |
| 2304x1296 synthetic probe | 170 | 170 inferred, 0 failed | 13.54 ms | P010 packing verified |
| 2560x1440 synthetic probe | 170 | 170 inferred, 0 failed | 16.38 ms | P010 packing verified |
| 3840x2160 synthetic probe | 170 | 170 inferred, 0 failed | 40.29 ms | P010 packing verified |
| 3840x2160 SDR real film, short product playback | 630 | 626 inferred, 0 failed | 37.89 ms | 4 hard cuts copied from F0 |

Additional focused checks:

- The real-film sequence at approximately 31:00 produced 61 pairs, detected
  exactly three hard cuts, copied F0 for those midpoints, inferred the other 58
  pairs, and reported no failure.
- Two exact seeks triggered two filter resets. Playback resumed after each
  reset and completed 80/80 midpoint inferences without failure.
- The short 4K product playback initialized the runtime in 7.27 seconds and
  logged 626 inferred midpoint samples with a 34.35 ms average. This sample is
  about 26 seconds long and does not replace the required 10-minute test.
- A repeat after the final product rebuild logged 216 inferred midpoints with
  no failure, a 34.33 ms average, and a 41.25 ms p95. That p95 leaves only
  0.42 ms below the 41.67 ms interval of an exact 24 FPS source.

The 4K p95 is only 1.42 ms below the 41.71 ms source-frame interval required for
23.976 to 47.952 FPS strict x2 output. It passes the short steady-state probe but
does not have enough margin to call 4K production-stable. First-use engine
initialization still produces a visible startup cost. Long-form real-film
playback, subtitle composition, audio/video drift, display cadence, thermal
stability, and recovery after repeated seeks still require explicit acceptance
testing.

## Profiled 4K cost distribution

Runtime ABI 4 adds an explicit probe-only stage profiler. The product filter
sets `profiling_enabled=0`, so normal playback does not add the profiler's D3D11
or CUDA synchronization points. The probe uses D3D11 completion queries around
both conversion shaders and CUDA events around TensorRT. Upload completion is
isolated before each sample, and the first TensorRT warm-up inference is excluded
from steady-state statistics.

The following profile used 62 consecutive inferred frame pairs from a real
3840x2160 P010 film sequence around 31:00 on the RTX 5070 Ti reference machine:

| Stage | Average | p95 | Maximum | Share of total |
| --- | ---: | ---: | ---: | ---: |
| Total runtime process | 37.445 ms | 39.800 ms | 41.062 ms | 100.00% |
| Frame validation and views | 0.028 ms | 0.025 ms | 0.040 ms | 0.07% |
| Scene detection and readback | 0.323 ms | 0.850 ms | 0.879 ms | 0.86% |
| P010 to FP16 input conversion | 0.410 ms | 0.700 ms | 1.067 ms | 1.09% |
| D3D11 to CUDA map | 0.269 ms | 0.525 ms | 0.983 ms | 0.72% |
| Tensor address binding | 0.005 ms | <0.025 ms | 0.020 ms | 0.01% |
| TensorRT-RTX RIFE inference | 35.897 ms | 38.200 ms | 38.729 ms | 95.86% |
| CUDA to D3D11 unmap | 0.263 ms | 0.525 ms | 0.694 ms | 0.70% |
| FP16 output to P010 conversion | 0.221 ms | 0.250 ms | 0.323 ms | 0.59% |
| Unattributed control overhead | 0.031 ms | n/a | n/a | 0.08% |

A separate 300-pair synthetic 4K run measured 35.785 ms average and 36.550 ms
p95 total time; TensorRT accounted for 34.481 ms, or 96.36%. Both workloads
therefore identify the full-resolution RIFE TensorRT graph as the dominant 4K
bottleneck. P010 conversion, D3D11/CUDA interoperability, scene detection, and
host control combined account for approximately 1.55 ms on the real sequence.
The NVOFA execution path is not enabled by the product RIFE mode and contributes
no time to this profile.

The first inference before warm-up took 107.582 ms in the real-sequence probe.
That is a separate first-use latency issue and is not included in the steady-state
table. This short profile establishes cost attribution only; it does not replace
the required long-form stability acceptance test.

## Real-film quality comparison

The active model was changed from v4.25 Lite to v4.26 after comparing both the
official vs-mlrt graph and the native runtime on 62 consecutive 1920x1080 P010
frames around 31:00 of a 23.976 FPS SDR film. The Lite result was not a native
tensor-layout defect: official and native Lite outputs showed the same temporal
bias toward F0.

| Pair | Model / path | Delta to F0 | Delta to F1 | Temporal imbalance |
| ---: | --- | ---: | ---: | ---: |
| 34 | v4.25 Lite native | 6.406 | 15.546 | 0.416 |
| 34 | v4.26 native | 10.279 | 11.115 | 0.039 |
| 34 | v4.26 official | 10.408 | 10.374 | 0.002 |
| 48 | v4.25 Lite native | 6.950 | 16.717 | 0.413 |
| 48 | v4.26 native | 10.276 | 10.111 | 0.008 |
| 48 | v4.26 official | 10.394 | 9.694 | 0.035 |

The native v4.26 output differs from the official v4.26 output by an average of
2.56 luma codes out of 1023 across normal pairs. This remaining difference is
consistent with the native P010/RGB shader conversion versus the reference
zimg conversion; it does not reproduce the Lite temporal bias.

## Earlier isolated probe

The original VapourSynth playback path was not suitable for full-resolution 4K
P010 playback. On
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

These measurements established the native conversion, interop, inference, and
P010 packing budget before the bridge was integrated into mpv. The integrated
results above supersede them for decode/filter/output and seek/reset behavior;
the stated long-form acceptance gaps remain.
