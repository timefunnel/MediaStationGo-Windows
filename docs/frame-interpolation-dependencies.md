# RTX frame interpolation dependency record

This document records the first development baseline. It is not a redistribution approval.

| Component | Pinned baseline | License / distribution status |
| --- | --- | --- |
| VapourSynth | R65 (`mingw-w64-clang-x86_64-vapoursynth` 65-8) | LGPL-2.1-or-later; MSYS2 package metadata verified locally |
| vs-mlrt scripts and VSTRT-RTX plugin | v15.16 | GPL-3.0; source and notices must accompany any compliant distribution |
| TensorRT-RTX | 1.4.0.76, CUDA 13.2 build | NVIDIA proprietary SDK; redistribution rights must be reviewed and confirmed before publishing binaries |
| RIFE model | v4.25 Lite, implementation 1 ONNX | Supplied by the vs-mlrt external-models release; redistribution provenance must be confirmed before publishing |
| onnxconverter-common | 1.16.0 | Apache-2.0 |

Development artifact hashes:

- `VSTRT-RTX-Windows-x64.v15.16.7z`: `d2d311b09635d6681285aa4eb30030b953ed243b49c6d842c490d32c338ca303`
- `scripts.v15.16.7z`: `d07dae0a00cb8dbf4f00358f640f630ff5d933de44d27050e7acce4f31cc3560`
- `rife_v4.25_lite.7z`: `7d53e29fff5e67345b19f4ce97dfd1e34b490eedc752df2236904fda1a13842c`
- `TensorRT-RTX-1.4.0.76-Windows-amd64-cuda-13.2-Release-external.zip`: `0a050b10158bbe286c90b55b23dffbd3d5096c626b2ee45eccf51322795a3c29`
- `onnxconverter_common-1.16.0-py2.py3-none-any.whl`: `df39ee96f17fff119dff10dd245467651b60b9e8a96020eb93402239794852f7`

The preparation script requires an explicit `-AcceptNvidiaLicense` switch and a caller-provided TensorRT-RTX archive. It never downloads the NVIDIA SDK itself. The generated runtime stays under ignored `third_party/` and its manifest marks it as non-redistributable.

## Known baseline constraint

vs-mlrt v15.16 explicitly rejects `scale != 1.0` for RIFE v4.25 Lite in `RIFEMerge`. The first PoC therefore validates only 1920x1080 at `scale=1.0`. The proposed 4K `scale=0.5` path must be resolved and measured separately; it is not silently replaced by another interpolator.
