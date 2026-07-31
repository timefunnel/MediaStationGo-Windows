"""Convert a dynamic FP32 RIFE ONNX graph to full FP16 compute."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto, helper, numpy_helper
from onnxconverter_common.float16 import convert_float_to_float16


INPUT_CHANNELS = 11
OUTPUT_CHANNELS = 3
VALIDATION_SIZE = 128
MAX_ABSOLUTE_ERROR = 1.5e-2
MEAN_ABSOLUTE_ERROR = 1.0e-3
LITE_MODEL_ID = "rife-v4.25-lite"
LITE_WARP_RESHAPE_COUNT = 4


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def tensor_shape(value: onnx.ValueInfoProto) -> tuple[int | str, ...]:
    return tuple(
        dimension.dim_value or dimension.dim_param
        for dimension in value.type.tensor_type.shape.dim
    )


def require_source_contract(model: onnx.ModelProto) -> tuple[str, str]:
    if len(model.graph.input) != 1 or len(model.graph.output) != 1:
        raise RuntimeError("RIFE ONNX must have exactly one input and one output")
    graph_input = model.graph.input[0]
    graph_output = model.graph.output[0]
    input_type = graph_input.type.tensor_type.elem_type
    output_type = graph_output.type.tensor_type.elem_type
    if input_type != TensorProto.FLOAT or output_type != TensorProto.FLOAT:
        raise RuntimeError(
            f"RIFE source ONNX IO must be FP32, got input={input_type}, "
            f"output={output_type}"
        )
    input_shape = tensor_shape(graph_input)
    output_shape = tensor_shape(graph_output)
    if len(input_shape) != 4 or input_shape[:2] != (1, INPUT_CHANNELS):
        raise RuntimeError(f"RIFE source input shape is invalid: {input_shape}")
    if len(output_shape) != 4 or output_shape[:2] != (1, OUTPUT_CHANNELS):
        raise RuntimeError(f"RIFE source output shape is invalid: {output_shape}")
    if not all(isinstance(value, str) or value == -1 for value in input_shape[2:]):
        raise RuntimeError(f"RIFE source input shape is not dynamic: {input_shape}")
    if not all(isinstance(value, str) or value == -1 for value in output_shape[2:]):
        raise RuntimeError(f"RIFE source output shape is not dynamic: {output_shape}")
    return graph_input.name, graph_output.name


def require_fp16_contract(
    model: onnx.ModelProto,
    allowed_fp32_initializers: frozenset[str] = frozenset(),
) -> tuple[str, str]:
    if len(model.graph.input) != 1 or len(model.graph.output) != 1:
        raise RuntimeError("RIFE FP16 ONNX must have exactly one input and one output")
    graph_input = model.graph.input[0]
    graph_output = model.graph.output[0]
    io_types = (
        graph_input.type.tensor_type.elem_type,
        graph_output.type.tensor_type.elem_type,
    )
    if io_types != (TensorProto.FLOAT16, TensorProto.FLOAT16):
        raise RuntimeError(f"RIFE converted ONNX IO must be FP16, got {io_types}")
    input_shape = tensor_shape(graph_input)
    output_shape = tensor_shape(graph_output)
    if (
        len(input_shape) != 4
        or input_shape[:2] != (1, INPUT_CHANNELS)
        or len(output_shape) != 4
        or output_shape[:2] != (1, OUTPUT_CHANNELS)
    ):
        raise RuntimeError(
            f"RIFE converted ONNX shapes are invalid: input={input_shape}, "
            f"output={output_shape}"
        )
    if not all(isinstance(value, str) or value == -1 for value in input_shape[2:]):
        raise RuntimeError(f"RIFE converted input shape is not dynamic: {input_shape}")
    if not all(isinstance(value, str) or value == -1 for value in output_shape[2:]):
        raise RuntimeError(f"RIFE converted output shape is not dynamic: {output_shape}")
    fp32_initializers = {
        value.name
        for value in model.graph.initializer
        if value.data_type == TensorProto.FLOAT
    }
    if fp32_initializers != allowed_fp32_initializers:
        raise RuntimeError(
            "RIFE converted ONNX has an unexpected FP32 initializer set: "
            f"expected={sorted(allowed_fp32_initializers)}, "
            f"actual={sorted(fp32_initializers)}"
        )
    if not any(
        value.data_type == TensorProto.FLOAT16 for value in model.graph.initializer
    ):
        raise RuntimeError("RIFE converted ONNX has no FP16 initializers")
    return graph_input.name, graph_output.name


def set_metadata(model: onnx.ModelProto, values: dict[str, str]) -> None:
    metadata = {entry.key: entry.value for entry in model.metadata_props}
    metadata.update(values)
    model.metadata_props.clear()
    for key in sorted(metadata):
        entry = model.metadata_props.add()
        entry.key = key
        entry.value = metadata[key]


def constant_value(node: onnx.NodeProto) -> np.ndarray | None:
    if node.op_type != "Constant":
        return None
    attributes = [value for value in node.attribute if value.name == "value"]
    if len(attributes) != 1 or not attributes[0].HasField("t"):
        return None
    return numpy_helper.to_array(attributes[0].t)


def normalize_lite_warp_batch_reshapes(model: onnx.ModelProto) -> int:
    """Avoid a TensorRT RTX fusion bug without changing Lite model values."""

    producers = {
        output: node
        for node in model.graph.node
        for output in node.output
        if output
    }
    grid_outputs = {
        output
        for node in model.graph.node
        if node.op_type == "GridSample"
        for output in node.output
    }
    targets: dict[str, tuple[str, str]] = {}
    obsolete_constants: set[str] = set()
    for node in model.graph.node:
        if (
            node.op_type != "Reshape"
            or len(node.input) != 2
            or len(node.output) != 1
            or node.input[0] not in grid_outputs
        ):
            continue
        shape_node = producers.get(node.input[1])
        shape = constant_value(shape_node) if shape_node is not None else None
        if shape is None or shape.tolist() != [1, 14, 0, 0]:
            continue
        shape_consumers = sum(
            input_name == node.input[1]
            for candidate in model.graph.node
            for input_name in candidate.input
        )
        if shape_consumers != 1:
            raise RuntimeError(
                f"Lite warp Reshape shape has {shape_consumers} consumers: {node.name}"
            )
        targets[node.output[0]] = (node.input[0], node.input[1])
        obsolete_constants.add(node.input[1])

    if len(targets) != LITE_WARP_RESHAPE_COUNT:
        raise RuntimeError(
            "RIFE v4.25 Lite graph does not contain the expected warp Reshapes: "
            f"expected={LITE_WARP_RESHAPE_COUNT}, actual={len(targets)}"
        )

    rewritten_nodes: list[onnx.NodeProto] = []
    rewrite_index = 0
    for node in model.graph.node:
        if (
            node.op_type == "Constant"
            and len(node.output) == 1
            and node.output[0] in obsolete_constants
        ):
            continue
        target = targets.get(node.output[0]) if len(node.output) == 1 else None
        if target is None:
            rewritten_nodes.append(node)
            continue

        rewrite_index += 1
        source, _ = target
        split_sizes = f"MediaStationWarpBatchSplit{rewrite_index}Sizes"
        batch0 = f"{node.output[0]}__batch0"
        batch1 = f"{node.output[0]}__batch1"
        model.graph.initializer.append(
            numpy_helper.from_array(
                np.asarray([1, 1], dtype=np.int64),
                split_sizes,
            )
        )
        rewritten_nodes.extend(
            (
                helper.make_node(
                    "Split",
                    [source, split_sizes],
                    [batch0, batch1],
                    name=f"MediaStationWarpBatchSplit{rewrite_index}",
                    axis=0,
                ),
                helper.make_node(
                    "Concat",
                    [batch0, batch1],
                    [node.output[0]],
                    name=f"MediaStationWarpBatchConcat{rewrite_index}",
                    axis=1,
                ),
            )
        )

    model.graph.ClearField("node")
    model.graph.node.extend(rewritten_nodes)
    return rewrite_index


def convert_model(
    source_path: Path,
    output_path: Path,
    model_id: str,
    scale: str,
    alignment: int,
) -> int:
    model = onnx.load(str(source_path))
    onnx.checker.check_model(model)
    input_name, output_name = require_source_contract(model)
    model = convert_float_to_float16(model, keep_io_types=False)
    converted_input, converted_output = require_fp16_contract(model)
    if (converted_input, converted_output) != (input_name, output_name):
        raise RuntimeError("RIFE FP16 conversion changed the runtime IO names")
    normalization_count = 0
    if model_id == LITE_MODEL_ID:
        normalization_count = normalize_lite_warp_batch_reshapes(model)
    set_metadata(
        model,
        {
            "mediastation_engine_shape": "dynamic",
            "mediastation_input_contract": "fp16[1,11,H,W]",
            "mediastation_model_id": model_id,
            "mediastation_output_contract": "fp16[1,3,H,W]",
            "mediastation_precision": "fp16_compute",
            "mediastation_scale": scale,
            "mediastation_shape_alignment": str(alignment),
            "mediastation_source_sha256": sha256(source_path),
            "mediastation_converter": "rife_fp16_io_wrapper.py",
            "mediastation_graph_normalization": (
                "warp_batch_split_concat" if normalization_count else "none"
            ),
        },
    )
    onnx.checker.check_model(model)

    temporary_output = output_path.with_suffix(f"{output_path.suffix}.tmp")
    temporary_output.unlink(missing_ok=True)
    try:
        onnx.save(model, str(temporary_output))
        verify_numerics(source_path, temporary_output, input_name, output_name)
        temporary_output.replace(output_path)
    finally:
        temporary_output.unlink(missing_ok=True)
    return normalization_count


def validation_input(
    seed: int,
    timestep: float,
    height: int = VALIDATION_SIZE,
    width: int = VALIDATION_SIZE,
) -> np.ndarray:
    random = np.random.default_rng(seed)
    vertical_coordinate = np.linspace(0.0, 1.0, height, dtype=np.float32)
    horizontal_coordinate = np.linspace(0.0, 1.0, width, dtype=np.float32)
    vertical, horizontal = np.meshgrid(
        vertical_coordinate,
        horizontal_coordinate,
        indexing="ij",
    )
    frame0 = np.stack(
        (
            0.5 + 0.22 * np.sin(horizontal * 13 + vertical * 5)
            + 0.12 * np.cos(vertical * 29),
            0.15 + 0.65 * horizontal + 0.12 * np.sin(vertical * 19),
            0.2 + 0.55 * vertical + 0.1 * np.cos(horizontal * 31 + vertical * 7),
        ),
        axis=0,
    ).astype(np.float32)
    frame0 = np.clip(
        frame0 + random.normal(0.0, 0.015, frame0.shape).astype(np.float32),
        0.0,
        1.0,
    )[np.newaxis]
    frame1 = np.roll(frame0, shift=seed % 3 + 1, axis=3)
    frame1 = np.clip(frame1 * 0.997 + 0.002, 0.0, 1.0)
    values = np.zeros(
        (1, INPUT_CHANNELS, height, width), dtype=np.float32
    )
    values[:, 0:3] = frame0
    values[:, 3:6] = frame1
    values[:, 6:7].fill(timestep)
    horizontal_grid = np.linspace(-1.0, 1.0, width, dtype=np.float32)
    vertical_grid = np.linspace(-1.0, 1.0, height, dtype=np.float32)
    values[:, 7:8] = horizontal_grid.reshape(1, 1, 1, -1)
    values[:, 8:9] = vertical_grid.reshape(1, 1, -1, 1)
    values[:, 9:10].fill(2.0 / (width - 1))
    values[:, 10:11].fill(2.0 / (height - 1))
    return values


def verify_numerics(
    source_path: Path,
    wrapped_path: Path,
    input_name: str,
    output_name: str,
) -> tuple[float, float]:
    source = ort.InferenceSession(str(source_path), providers=["CPUExecutionProvider"])
    wrapped = ort.InferenceSession(str(wrapped_path), providers=["CPUExecutionProvider"])
    max_error = 0.0
    error_sum = 0.0
    error_count = 0
    for index, timestep in enumerate((0.25, 0.5, 0.75)):
        values = validation_input(20260731 + index, timestep)
        reference = source.run([output_name], {input_name: values})[0]
        actual = wrapped.run(
            [output_name], {input_name: values.astype(np.float16)}
        )[0]
        difference = np.abs(actual.astype(np.float32) - reference.astype(np.float32))
        max_error = max(max_error, float(difference.max()))
        error_sum += float(difference.sum(dtype=np.float64))
        error_count += difference.size
    mean_error = error_sum / error_count
    if max_error > MAX_ABSOLUTE_ERROR or mean_error > MEAN_ABSOLUTE_ERROR:
        raise RuntimeError(
            f"Full FP16 conversion validation failed: max_abs={max_error:.8f}, "
            f"mean_abs={mean_error:.8f}, max_limit={MAX_ABSOLUTE_ERROR:.8f}, "
            f"mean_limit={MEAN_ABSOLUTE_ERROR:.8f}"
        )
    return max_error, mean_error


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--model-id", required=True)
    parser.add_argument("--scale", choices=("0.5", "1.0"), required=True)
    parser.add_argument("--alignment", type=int, choices=(64, 128), required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if not args.input.is_file():
        raise FileNotFoundError(f"RIFE source ONNX is missing: {args.input}")
    if args.input.resolve() == args.output.resolve():
        raise ValueError("RIFE source and wrapped output paths must differ")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    normalization_count = convert_model(
        args.input, args.output, args.model_id, args.scale, args.alignment
    )
    model = onnx.load(str(args.output))
    input_name = model.graph.input[0].name
    output_name = model.graph.output[0].name
    max_error, mean_error = verify_numerics(
        args.input, args.output, input_name, output_name
    )
    print(
        json.dumps(
            {
                "input_sha256": sha256(args.input),
                "model_id": args.model_id,
                "output_path": str(args.output),
                "output_sha256": sha256(args.output),
                "scale": args.scale,
                "shape_alignment": args.alignment,
                "status": "RIFE_FULL_FP16_MODEL_OK",
                "fp32_to_fp16_max_abs": max_error,
                "fp32_to_fp16_mean_abs": mean_error,
                "warp_batch_normalizations": normalization_count,
            },
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()
