import os
from fractions import Fraction

import vapoursynth as vs

core = vs.core
core.std.LoadPlugin(path=os.environ["MSGO_VSMLRT_PLUGIN"])

# MSYS2's R65 binding exposes enum classes but omits the legacy aliases still
# consumed by vs-mlrt v15.16. Keep the compatibility layer local and explicit.
vs.FLOAT = vs.SampleType.FLOAT
vs.GRAY = vs.ColorFamily.GRAY
vs.RGB = vs.ColorFamily.RGB
vs.YUV = vs.ColorFamily.YUV
vs.GRAYH = core.query_video_format(vs.GRAY, vs.FLOAT, 16, 0, 0).id
vs.GRAYS = core.query_video_format(vs.GRAY, vs.FLOAT, 32, 0, 0).id
vs.RGBH = core.query_video_format(vs.RGB, vs.FLOAT, 16, 0, 0).id

from vsmlrt import Backend, RIFE, RIFEModel


width = int(os.environ.get("MSGO_RIFE_WIDTH", "1920"))
height = int(os.environ.get("MSGO_RIFE_HEIGHT", "1080"))
source_fps = Fraction(os.environ.get("MSGO_RIFE_SOURCE_FPS", "24"))
target_fps = Fraction(os.environ.get("MSGO_RIFE_TARGET_FPS", "60"))
frame_count = int(os.environ.get("MSGO_RIFE_SOURCE_FRAMES", "48"))
engine_folder = os.environ["MSGO_RIFE_ENGINE_DIR"]

if width != 1920 or height != 1080:
    raise ValueError("The first PoC is intentionally limited to 1920x1080")
if target_fps <= source_fps:
    raise ValueError("Target FPS must exceed source FPS")

base = core.std.BlankClip(
    width=width,
    height=height,
    format=vs.RGBH,
    length=frame_count,
    fpsnum=source_fps.numerator,
    fpsden=source_fps.denominator,
    color=[0.025, 0.035, 0.045],
)
marker_width = 320
marker_height = 180
marker = core.std.BlankClip(
    base,
    width=marker_width,
    height=marker_height,
    color=[0.85, 0.22, 0.08],
)


def moving_marker(n):
    left = (n * 31) % (width - marker_width)
    top = (n * 17) % (height - marker_height)
    return core.std.AddBorders(
        marker,
        left=left,
        right=width - marker_width - left,
        top=top,
        bottom=height - marker_height - top,
        color=[0.025, 0.035, 0.045],
    )


source = core.std.FrameEval(base, moving_marker)
padding_multiple = 128
padded_width = ((width + padding_multiple - 1) // padding_multiple) * padding_multiple
padded_height = ((height + padding_multiple - 1) // padding_multiple) * padding_multiple
source_for_rife = core.std.AddBorders(
    source,
    right=padded_width - width,
    bottom=padded_height - height,
    color=[0.025, 0.035, 0.045],
)
backend = Backend.TRT_RTX(
    fp16=True,
    use_cuda_graph=True,
    num_streams=1,
    engine_folder=engine_folder,
)
interpolated = RIFE(
    source_for_rife,
    multi=target_fps / source_fps,
    scale=1.0,
    model=RIFEModel.v4_25_lite,
    backend=backend,
    video_player=True,
)
output = core.std.CropAbs(interpolated, width=width, height=height)
output.set_output()
