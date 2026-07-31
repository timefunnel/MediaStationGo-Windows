"""Wrap a dynamic FP32 RIFE ONNX graph with FP16 input and output casts."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto, helper


INPUT_CHANNELS = 11
OUTPUT_CHANNELS = 3
VALIDATION_SIZE = 128


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


def replace_name(model: onnx.ModelProto, old: str, new: str) -> None:
    for node in model.graph.node:
        for index, value in enumerate(node.input):
            if value == old:
                node.input[index] = new
        for index, value in enumerate(node.output):
            if value == old:
                node.output[index] = new
    for value in model.graph.value_info:
        if value.name == old:
            value.name = new


def set_metadata(model: onnx.ModelProto, values: dict[str, str]) -> None:
    metadata = {entry.key: entry.value for entry in model.metadata_props}
    metadata.update(values)
    model.metadata_props.clear()
    for key in sorted(metadata):
        entry = model.metadata_props.add()
        entry.key = key
        entry.value = metadata[key]


def wrap_model(
    source_path: Path,
    output_path: Path,
    model_id: str,
    scale: str,
    alignment: int,
) -> None:
    model = onnx.load(str(source_path))
    onnx.checker.check_model(model)
    input_name, output_name = require_source_contract(model)
    internal_input = f"{input_name}__mediastation_fp32"
    internal_output = f"{output_name}__mediastation_fp32"
    existing_names = {
        value
        for node in model.graph.node
        for value in (*node.input, *node.output)
    }
    if internal_input in existing_names or internal_output in existing_names:
        raise RuntimeError("RIFE source ONNX already uses MediaStation wrapper names")

    replace_name(model, input_name, internal_input)
    replace_name(model, output_name, internal_output)
    model.graph.input[0].name = input_name
    model.graph.input[0].type.tensor_type.elem_type = TensorProto.FLOAT16
    model.graph.output[0].name = output_name
    model.graph.output[0].type.tensor_type.elem_type = TensorProto.FLOAT16
    input_cast = helper.make_node(
        "Cast",
        [input_name],
        [internal_input],
        name="MediaStationInputFp16ToFp32",
        to=TensorProto.FLOAT,
    )
    output_cast = helper.make_node(
        "Cast",
        [internal_output],
        [output_name],
        name="MediaStationOutputFp32ToFp16",
        to=TensorProto.FLOAT16,
    )
    original_nodes = list(model.graph.node)
    model.graph.ClearField("node")
    model.graph.node.extend([input_cast, *original_nodes, output_cast])
    set_metadata(
        model,
        {
            "mediastation_engine_shape": "dynamic",
            "mediastation_input_contract": "fp16[1,11,H,W]",
            "mediastation_model_id": model_id,
            "mediastation_output_contract": "fp16[1,3,H,W]",
            "mediastation_precision": "fp16_io_fp32_compute",
            "mediastation_scale": scale,
            "mediastation_shape_alignment": str(alignment),
            "mediastation_source_sha256": sha256(source_path),
            "mediastation_wrapper": "rife_fp16_io_wrapper.py",
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


def validation_input(seed: int, timestep: float) -> np.ndarray:
    values = np.random.default_rng(seed).random(
        (1, INPUT_CHANNELS, VALIDATION_SIZE, VALIDATION_SIZE), dtype=np.float32
    )
    values[:, 6:7].fill(timestep)
    horizontal = np.linspace(-1.0, 1.0, VALIDATION_SIZE, dtype=np.float32)
    vertical = np.linspace(-1.0, 1.0, VALIDATION_SIZE, dtype=np.float32)
    values[:, 7:8] = horizontal.reshape(1, 1, 1, -1)
    values[:, 8:9] = vertical.reshape(1, 1, -1, 1)
    values[:, 9:10].fill(2.0 / (VALIDATION_SIZE - 1))
    values[:, 10:11].fill(2.0 / (VALIDATION_SIZE - 1))
    return values.astype(np.float16)


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
        reference = source.run([output_name], {input_name: values.astype(np.float32)})[0]
        reference = reference.astype(np.float16)
        actual = wrapped.run([output_name], {input_name: values})[0]
        difference = np.abs(actual.astype(np.float32) - reference.astype(np.float32))
        max_error = max(max_error, float(difference.max()))
        error_sum += float(difference.sum(dtype=np.float64))
        error_count += difference.size
    mean_error = error_sum / error_count
    if max_error > 1.0e-3 or mean_error > 1.0e-4:
        raise RuntimeError(
            f"FP16 IO wrapper validation failed: max_abs={max_error:.8f}, "
            f"mean_abs={mean_error:.8f}"
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
    wrap_model(args.input, args.output, args.model_id, args.scale, args.alignment)
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
                "status": "RIFE_FP16_IO_WRAPPER_OK",
                "wrapper_max_abs": max_error,
                "wrapper_mean_abs": mean_error,
            },
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()
