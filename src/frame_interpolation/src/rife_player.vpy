import base64
import json
import os
import sys
from fractions import Fraction

import vapoursynth as vs


params = json.loads(base64.b64decode(user_data).decode("utf-8"))
core = vs.core
core.std.LoadPlugin(path=params["plugin_path"])

# MSYS2 R65 exposes enum classes but omits aliases used by vs-mlrt v15.16.
vs.FLOAT = vs.SampleType.FLOAT
vs.GRAY = vs.ColorFamily.GRAY
vs.RGB = vs.ColorFamily.RGB
vs.YUV = vs.ColorFamily.YUV
vs.GRAYH = core.query_video_format(vs.GRAY, vs.FLOAT, 16, 0, 0).id
vs.GRAYS = core.query_video_format(vs.GRAY, vs.FLOAT, 32, 0, 0).id
vs.RGBH = core.query_video_format(vs.RGB, vs.FLOAT, 16, 0, 0).id

sys.path.insert(0, params["scripts_path"])
from vsmlrt import Backend, RIFE, RIFEModel


source_fps = Fraction(params["source_fps_num"], params["source_fps_den"])
target_fps = Fraction(params["target_fps"], 1)
if source_fps <= 0 or target_fps <= source_fps:
    raise ValueError("MSGO_RIFE_INVALID_FRAME_RATE")
if video_in.width != params["width"] or video_in.height != params["height"]:
    raise ValueError("MSGO_RIFE_SOURCE_DIMENSIONS_CHANGED")

source = core.std.AssumeFPS(
    video_in,
    fpsnum=source_fps.numerator,
    fpsden=source_fps.denominator,
)
source_format = source.format
if source_format is None:
    raise ValueError("MSGO_RIFE_VARIABLE_SOURCE_FORMAT")

range_name = params["color_range"]
if source_format.color_family == vs.RGB:
    rgb = core.resize.Bicubic(
        source,
        format=vs.RGBH,
        range_in_s=range_name,
        range_s="full",
    )
else:
    rgb = core.resize.Bicubic(
        source,
        format=vs.RGBH,
        matrix_in_s=params["matrix"],
        range_in_s=range_name,
        range_s="full",
    )

padding_multiple = 128
padded_width = ((source.width + padding_multiple - 1) // padding_multiple) * padding_multiple
padded_height = ((source.height + padding_multiple - 1) // padding_multiple) * padding_multiple
padded = core.std.AddBorders(
    rgb,
    right=padded_width - source.width,
    bottom=padded_height - source.height,
)
backend = Backend.TRT_RTX(
    fp16=True,
    use_cuda_graph=True,
    num_streams=1,
    engine_folder=params["engine_path"],
)
interpolated = RIFE(
    padded,
    multi=target_fps / source_fps,
    scale=params["scale"],
    model=RIFEModel.v4_25_lite,
    backend=backend,
    video_player=True,
)
cropped = core.std.CropAbs(interpolated, width=source.width, height=source.height)

if source_format.color_family == vs.RGB:
    converted = core.resize.Bicubic(
        cropped,
        format=source_format.id,
        range_in_s="full",
        range_s=range_name,
    )
else:
    converted = core.resize.Bicubic(
        cropped,
        format=source_format.id,
        matrix_s=params["matrix"],
        range_in_s="full",
        range_s=range_name,
    )

# RIFE creates new frames. Copy all source props, including mpv's private
# _MP_IMAGE blob and HDR static metadata, from the corresponding left frame.
duration_rel = source_fps / target_fps
dst_duration = duration_rel.numerator
src_duration = duration_rel.denominator
props_template = core.std.BlankClip(source, length=converted.num_frames, keep=True)


def source_props(n):
    return source[dst_duration * n // src_duration]


props = core.std.FrameEval(props_template, source_props)
output = core.std.CopyFrameProps(converted, props)
output = core.std.SetFrameProps(
    output,
    _DurationNum=target_fps.denominator,
    _DurationDen=target_fps.numerator,
)
output.set_output()
