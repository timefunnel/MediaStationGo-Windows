import os
from fractions import Fraction

import vapoursynth as vs

core = vs.core
core.std.LoadPlugin(path=os.environ["MSGO_VSMLRT_PLUGIN"])

# MSYS2's R65 binding exposes enum classes but omits the legacy aliases still
# consumed by vs-mlrt v15.16. Keep the compatibility layer local and explicit.
vs.FLOAT = vs.SampleType.FLOAT
vs.INTEGER = vs.SampleType.INTEGER
vs.GRAY = vs.ColorFamily.GRAY
vs.RGB = vs.ColorFamily.RGB
vs.YUV = vs.ColorFamily.YUV
vs.GRAYH = core.query_video_format(vs.GRAY, vs.FLOAT, 16, 0, 0).id
vs.GRAYS = core.query_video_format(vs.GRAY, vs.FLOAT, 32, 0, 0).id
vs.RGBH = core.query_video_format(vs.RGB, vs.FLOAT, 16, 0, 0).id

from vsmlrt import Backend, RIFE, RIFEModel


width = int(os.environ.get("MSGO_RIFE_WIDTH", "1920"))
height = int(os.environ.get("MSGO_RIFE_HEIGHT", "1080"))
inference_width = int(os.environ.get("MSGO_RIFE_INFERENCE_WIDTH", str(width)))
inference_height = int(os.environ.get("MSGO_RIFE_INFERENCE_HEIGHT", str(height)))
source_fps = Fraction(os.environ.get("MSGO_RIFE_SOURCE_FPS", "24"))
target_fps = Fraction(os.environ.get("MSGO_RIFE_TARGET_FPS", "60"))
frame_count = int(os.environ.get("MSGO_RIFE_SOURCE_FRAMES", "48"))
engine_folder = os.environ["MSGO_RIFE_ENGINE_DIR"]
scale = float(os.environ.get("MSGO_RIFE_SCALE", "1.0"))
streams = int(os.environ.get("MSGO_RIFE_STREAMS", "1"))
pipeline = os.environ.get("MSGO_RIFE_PIPELINE", "rgb")

if target_fps <= source_fps:
    raise ValueError("Target FPS must exceed source FPS")
if scale not in (0.5, 1.0):
    raise ValueError("Scale must be 0.5 or 1.0")
if streams not in (1, 2):
    raise ValueError("Streams must be 1 or 2")
if pipeline not in ("rgb", "yuv10"):
    raise ValueError("Pipeline must be rgb or yuv10")
if not (0 < inference_width <= width and 0 < inference_height <= height):
    raise ValueError("Inference dimensions must be positive and cannot exceed source dimensions")

working_format = vs.RGBH

base = core.std.BlankClip(
    width=width,
    height=height,
    format=working_format,
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
if pipeline == "yuv10":
    yuv420p10 = core.query_video_format(vs.YUV, vs.INTEGER, 10, 1, 1).id
    video_source = core.resize.Bicubic(
        source,
        format=yuv420p10,
        matrix_s="709",
        range_in_s="full",
        range_s="limited",
    )
    source_for_inference = core.resize.Bicubic(
        video_source,
        width=inference_width,
        height=inference_height,
        format=working_format,
        matrix_in_s="709",
        range_in_s="limited",
        range_s="full",
    )
else:
    source_for_inference = source
    if inference_width != width or inference_height != height:
        source_for_inference = core.resize.Bicubic(
            source,
            width=inference_width,
            height=inference_height,
        )
padding_multiple = 128
padded_width = ((inference_width + padding_multiple - 1) // padding_multiple) * padding_multiple
padded_height = ((inference_height + padding_multiple - 1) // padding_multiple) * padding_multiple
source_for_rife = core.std.AddBorders(
    source_for_inference,
    right=padded_width - inference_width,
    bottom=padded_height - inference_height,
    color=[0.025, 0.035, 0.045],
)
backend = Backend.TRT_RTX(
    fp16=True,
    use_cuda_graph=True,
    num_streams=streams,
    engine_folder=engine_folder,
)
interpolated = RIFE(
    source_for_rife,
    multi=target_fps / source_fps,
    scale=scale,
    model=RIFEModel.v4_25_lite,
    backend=backend,
    video_player=True,
)
output = core.std.CropAbs(interpolated, width=inference_width, height=inference_height)
if pipeline == "yuv10":
    output = core.resize.Bicubic(
        output,
        width=width,
        height=height,
        format=yuv420p10,
        matrix_s="709",
        range_in_s="full",
        range_s="limited",
    )
elif inference_width != width or inference_height != height:
    output = core.resize.Bicubic(output, width=width, height=height)
output.set_output()
