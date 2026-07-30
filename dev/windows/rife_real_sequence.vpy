import os
from pathlib import Path

import numpy as np
import vapoursynth as vs


core = vs.core
core.std.LoadPlugin(path=os.environ["MSGO_VSMLRT_PLUGIN"])

# MSYS2's R65 binding omits aliases still consumed by vs-mlrt v15.16.
vs.FLOAT = vs.SampleType.FLOAT
vs.INTEGER = vs.SampleType.INTEGER
vs.GRAY = vs.ColorFamily.GRAY
vs.RGB = vs.ColorFamily.RGB
vs.YUV = vs.ColorFamily.YUV
vs.GRAYH = core.query_video_format(vs.GRAY, vs.FLOAT, 16, 0, 0).id
vs.GRAYS = core.query_video_format(vs.GRAY, vs.FLOAT, 32, 0, 0).id
vs.RGBH = core.query_video_format(vs.RGB, vs.FLOAT, 16, 0, 0).id
vs.RGBS = core.query_video_format(vs.RGB, vs.FLOAT, 32, 0, 0).id

from vsmlrt import Backend, RIFE, RIFEModel


input_path = Path(os.environ["MSGO_RIFE_SEQUENCE_P010"])
engine_folder = os.environ["MSGO_RIFE_ENGINE_DIR"]
width = int(os.environ["MSGO_RIFE_WIDTH"])
height = int(os.environ["MSGO_RIFE_HEIGHT"])
frame_count = int(os.environ["MSGO_RIFE_SOURCE_FRAMES"])
fps_num = int(os.environ.get("MSGO_RIFE_FPS_NUM", "24000"))
fps_den = int(os.environ.get("MSGO_RIFE_FPS_DEN", "1001"))
model_name = os.environ.get("MSGO_RIFE_MODEL", "v4.26")

models = {
    "v4.25-lite": (RIFEModel.v4_25_lite, 128),
    "v4.26": (RIFEModel.v4_26, 64),
    "v4.26-heavy": (RIFEModel.v4_26_heavy, 64),
}
if model_name not in models:
    raise ValueError(f"unsupported RIFE sequence model: {model_name}")
model, padding_multiple = models[model_name]

if width <= 0 or height <= 0 or frame_count < 2:
    raise ValueError("invalid sequence dimensions or frame count")
if width % 2 or height % 2:
    raise ValueError("P010 input requires even dimensions")

frame_samples = width * height * 3 // 2
expected_size = frame_samples * frame_count * np.dtype("<u2").itemsize
if input_path.stat().st_size != expected_size:
    raise ValueError(
        f"P010 input size mismatch: expected {expected_size}, "
        f"received {input_path.stat().st_size}"
    )

raw_frames = np.memmap(
    input_path,
    dtype="<u2",
    mode="r",
    shape=(frame_count, frame_samples),
)
yuv420p10 = core.query_video_format(vs.YUV, vs.INTEGER, 10, 1, 1).id
base = core.std.BlankClip(
    width=width,
    height=height,
    format=yuv420p10,
    length=frame_count,
    fpsnum=fps_num,
    fpsden=fps_den,
)


def load_p010_frame(n, f):
    output = f.copy()
    packed = raw_frames[n]
    luma_samples = width * height
    luma = packed[:luma_samples].reshape(height, width)
    chroma = packed[luma_samples:].reshape(height // 2, width)
    np.copyto(np.asarray(output[0]), np.right_shift(luma, 6))
    np.copyto(np.asarray(output[1]), np.right_shift(chroma[:, 0::2], 6))
    np.copyto(np.asarray(output[2]), np.right_shift(chroma[:, 1::2], 6))
    return output


source_yuv = core.std.ModifyFrame(base, base, load_p010_frame)
source_rgb = core.resize.Bicubic(
    source_yuv,
    format=vs.RGBH,
    matrix_in_s="709",
    range_in_s="limited",
    range_s="full",
)

padded_width = ((width + padding_multiple - 1) // padding_multiple) * padding_multiple
padded_height = ((height + padding_multiple - 1) // padding_multiple) * padding_multiple
source_padded = core.std.AddBorders(
    source_rgb,
    right=padded_width - width,
    bottom=padded_height - height,
    color=[0.0, 0.0, 0.0],
)
backend = Backend.TRT_RTX(
    fp16=True,
    use_cuda_graph=True,
    num_streams=1,
    engine_folder=engine_folder,
)
interpolated = RIFE(
    source_padded,
    multi=2,
    scale=1.0,
    model=model,
    backend=backend,
    video_player=True,
    _implementation=1,
)
midpoints = core.std.SelectEvery(interpolated, cycle=2, offsets=1)
midpoints = core.std.Trim(midpoints, first=0, length=frame_count - 1)
midpoints = core.std.CropAbs(midpoints, width=width, height=height)
output = core.resize.Bicubic(
    midpoints,
    format=yuv420p10,
    matrix_s="709",
    range_in_s="full",
    range_s="limited",
)
output.set_output()
