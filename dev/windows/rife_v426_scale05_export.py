"""Export the official vs-rife v4.26 model at scale=0.5.

This is an export-time tool only. The generated graph keeps the native
rife_runtime.dll contract: FP16 [1, 11, H, W] -> [1, 3, H, W].
"""

from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import importlib
import json
import sys
import types
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
import torch
from onnxconverter_common.float16 import convert_float_to_float16
from torch import nn
from torch.nn import functional as F

from rife_fp16_io_wrapper import (
    require_fp16_contract,
    require_source_contract,
    set_metadata,
    validation_input,
)


UPSTREAM_COMMIT = "3488617283db7c428a83ba4a19382285da698b6a"
UPSTREAM_WEIGHT_SHA256 = "45c7f74156704769dc9f85cfcaf8552e1e926f9399dcfa3a553dee88fac6f53f"
UPSTREAM_IFNET_SHA256 = "0326ce02552c1c425517fdb2a6e9ffb23f174d092b0178fd9998f3d5036e607d"
UPSTREAM_WARP_SHA256 = "c3e47da7e968aa71c81cf2ce23c709e157cb5e07eab0f2de49df2a28aa2b1a14"
MODEL_ID = "rife-v4.26-scale0.5"
SCALE = 0.5
CHANNELS = 11
VALIDATION_SIZE = 128
DYNAMIC_VALIDATION_CASES = (
    (256, 384, 0.25),
    (512, 640, 0.5),
    (384, 896, 0.75),
)
EXPECTED_GRID_SAMPLE_COUNT = 5
EXPECTED_PRECISION_NODE_COUNT = 262
EXPECTED_FP32_OPERATOR_COUNT = 129
EXPECTED_FP32_CONVOLUTION_COUNT = 4
EXPECTED_FP32_INITIALIZERS = frozenset(
    {
        "model.encode.cnn0.weight",
        "model.encode.cnn0.bias",
        "model.encode.cnn2.weight",
        "model.encode.cnn2.bias",
    }
)
ENCODER_FP32_WEIGHTS = frozenset(
    {
        "model.encode.cnn0.weight",
        "model.encode.cnn2.weight",
    }
)
PRECISION_TRACE_OPERATORS = frozenset(
    {
        "Add",
        "Cast",
        "Clip",
        "Concat",
        "Constant",
        "ConstantOfShape",
        "DepthToSpace",
        "Div",
        "Expand",
        "Gather",
        "GridSample",
        "Mul",
        "Reciprocal",
        "Resize",
        "Sigmoid",
        "Slice",
        "Sub",
        "Tile",
        "Transpose",
        "Unsqueeze",
    }
)
EXPECTED_PRECISION_OPERATOR_COUNTS = Counter(
    {
        "Constant": 145,
        "Slice": 31,
        "Concat": 11,
        "Add": 10,
        "Div": 10,
        "Gather": 9,
        "Mul": 9,
        "DepthToSpace": 5,
        "GridSample": 5,
        "Resize": 5,
        "Transpose": 5,
        "Conv": 4,
        "LeakyRelu": 4,
        "Reciprocal": 2,
        "Unsqueeze": 2,
        "ConstantOfShape": 1,
        "Expand": 1,
        "Sigmoid": 1,
        "Sub": 1,
        "Tile": 1,
    }
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_upstream_ifnet(vs_rife_dir: Path, weight_path: Path) -> nn.Module:
    package_dir = vs_rife_dir / "vsrife"
    if not package_dir.is_dir():
        raise RuntimeError(f"vs-rife package directory is missing: {package_dir}")
    pinned_sources = {
        package_dir / "IFNet_HDv3_v4_26.py": UPSTREAM_IFNET_SHA256,
        package_dir / "warplayer.py": UPSTREAM_WARP_SHA256,
    }
    for source_path, expected in pinned_sources.items():
        if not source_path.is_file():
            raise RuntimeError(f"Pinned vs-rife source is missing: {source_path}")
        actual = sha256(source_path)
        if actual != expected:
            raise RuntimeError(
                f"Pinned vs-rife source SHA-256 mismatch for {source_path.name}: "
                f"expected={expected}, actual={actual}"
            )

    # Import only the pinned architecture modules. Importing vsrife.__init__
    # would pull VapourSynth and the player runtime into the export process.
    package = types.ModuleType("vsrife")
    package.__path__ = [str(package_dir)]
    sys.modules["vsrife"] = package
    module = importlib.import_module("vsrife.IFNet_HDv3_v4_26")

    model = module.IFNet(scale=SCALE, ensemble=False).eval()
    state_dict = torch.load(weight_path, map_location="cpu", weights_only=False)
    if not isinstance(state_dict, dict):
        raise RuntimeError("The official v4.26 weight is not a state dictionary")
    state_dict = {
        key.removeprefix("module."): value
        for key, value in state_dict.items()
        if key.startswith("module.")
    }
    if not state_dict:
        raise RuntimeError("The official v4.26 weight contains no module.* parameters")
    model_keys = set(model.state_dict())
    ignored = sorted(set(state_dict) - model_keys)
    unsupported = [
        key for key in ignored if not key.startswith(("teacher.", "caltime."))
    ]
    if unsupported:
        raise RuntimeError(
            "Official v4.26 weight contains unsupported parameters: "
            f"{unsupported}"
        )
    inference_state = {key: value for key, value in state_dict.items() if key in model_keys}
    model.load_state_dict(inference_state, strict=True)
    return model


def runtime_inputs(
    contract: torch.Tensor,
) -> tuple[torch.Tensor, torch.Tensor, torch.Tensor, torch.Tensor, torch.Tensor]:
    image0 = contract[:, 0:3]
    image1 = contract[:, 3:6]
    timestep = contract[:, 6:7]
    grid = contract[:, 7:9]
    flow_div = torch.stack(
        (1.0 / contract[0, 9, 0, 0], 1.0 / contract[0, 10, 0, 0])
    )
    return image0, image1, timestep, grid, flow_div


class OfficialRuntimeContractV426Scale05(nn.Module):
    """Adapt the native input to the pinned official vs-rife forward call."""

    def __init__(self, model: nn.Module) -> None:
        super().__init__()
        self.model = model

    def forward(self, contract: torch.Tensor) -> torch.Tensor:
        image0, image1, timestep, grid, flow_div = runtime_inputs(contract)
        feature0 = self.model.encode(image0)
        feature1 = self.model.encode(image1)
        return self.model(
            image0,
            image1,
            timestep,
            flow_div,
            grid,
            feature0,
            feature1,
        )


class RuntimeContractV426Scale05(nn.Module):
    """Batch equivalent warps so TensorRT receives five GridSample stages."""

    def __init__(self, model: nn.Module) -> None:
        super().__init__()
        self.model = model

    @staticmethod
    def warp_pair(
        sources: torch.Tensor,
        flow: torch.Tensor,
        flow_div: torch.Tensor,
        grid: torch.Tensor,
    ) -> torch.Tensor:
        pair_flow = torch.cat((flow[:, 0:2], flow[:, 2:4]), dim=0)
        normalized_flow = torch.cat(
            (
                pair_flow[:, 0:1] / flow_div[0],
                pair_flow[:, 1:2] / flow_div[1],
            ),
            dim=1,
        )
        sampling_grid = (grid.repeat(2, 1, 1, 1) + normalized_flow).permute(
            0, 2, 3, 1
        )
        return F.grid_sample(
            sources,
            sampling_grid,
            mode="bilinear",
            padding_mode="border",
            align_corners=True,
        )

    def forward(self, contract: torch.Tensor) -> torch.Tensor:
        image0, image1, timestep, grid, flow_div = runtime_inputs(contract)
        feature0 = self.model.encode(image0)
        feature1 = self.model.encode(image1)
        sources = torch.cat(
            (
                torch.cat((image0, feature0), dim=1),
                torch.cat((image1, feature1), dim=1),
            ),
            dim=0,
        )
        flow = None
        mask = None
        feature = None
        warped = None
        blocks = (
            self.model.block0,
            self.model.block1,
            self.model.block2,
            self.model.block3,
            self.model.block4,
        )
        for index, (block, scale) in enumerate(zip(blocks, self.model.scale_list)):
            if index == 0:
                flow, mask, feature = block(
                    torch.cat(
                        (image0, image1, feature0, feature1, timestep), dim=1
                    ),
                    None,
                    scale=scale,
                )
            else:
                warped_image0 = warped[0:1, 0:3]
                warped_image1 = warped[1:2, 0:3]
                warped_feature0 = warped[0:1, 3:7]
                warped_feature1 = warped[1:2, 3:7]
                flow_delta, mask, feature = block(
                    torch.cat(
                        (
                            warped_image0,
                            warped_image1,
                            warped_feature0,
                            warped_feature1,
                            timestep,
                            mask,
                            feature,
                        ),
                        dim=1,
                    ),
                    flow,
                    scale=scale,
                )
                flow = flow + flow_delta
            warped = self.warp_pair(sources, flow, flow_div, grid)
        mask = torch.sigmoid(mask)
        return warped[0:1, 0:3] * mask + warped[1:2, 0:3] * (1 - mask)


def make_validation_input(
    seed: int = 20260731,
    timestep: float = 0.5,
    height: int = VALIDATION_SIZE,
    width: int = VALIDATION_SIZE,
) -> torch.Tensor:
    return torch.from_numpy(validation_input(seed, timestep, height, width))


def verify_pytorch_compatibility(
    reference: nn.Module, wrapper: nn.Module
) -> tuple[float, float]:
    max_error = 0.0
    error_sum = 0.0
    error_count = 0
    for index, timestep in enumerate((0.25, 0.5, 0.75)):
        input_tensor = make_validation_input(20260731 + index, timestep)
        with torch.inference_mode():
            expected = reference(input_tensor).cpu().numpy()
            actual = wrapper(input_tensor).cpu().numpy()
        difference = np.abs(actual - expected)
        max_error = max(max_error, float(difference.max()))
        error_sum += float(difference.sum(dtype=np.float64))
        error_count += difference.size
    mean_error = error_sum / error_count
    if max_error > 1.0e-5 or mean_error > 1.0e-6:
        raise RuntimeError(
            "Batched warp compatibility validation failed: "
            f"max_abs={max_error:.8f}, mean_abs={mean_error:.8f}"
        )
    return max_error, mean_error


def remove_redundant_fp32_casts(model: onnx.ModelProto) -> int:
    inferred = onnx.shape_inference.infer_shapes(model)
    value_types = {
        value.name: value.type.tensor_type.elem_type
        for value in (
            *inferred.graph.input,
            *inferred.graph.output,
            *inferred.graph.value_info,
        )
    }
    value_types.update(
        {value.name: value.data_type for value in inferred.graph.initializer}
    )
    graph_outputs = {value.name for value in inferred.graph.output}
    replacements: dict[str, str] = {}
    kept_nodes = []
    for node in inferred.graph.node:
        if node.op_type != "Cast":
            kept_nodes.append(node)
            continue
        cast_types = [
            attribute.i for attribute in node.attribute if attribute.name == "to"
        ]
        if (
            len(node.input) != 1
            or len(node.output) != 1
            or cast_types != [onnx.TensorProto.FLOAT]
            or value_types.get(node.input[0]) != onnx.TensorProto.FLOAT
            or value_types.get(node.output[0]) != onnx.TensorProto.FLOAT
            or node.output[0] in graph_outputs
        ):
            raise RuntimeError(
                f"Scale=0.5 export contains a non-redundant Cast node: {node.name}"
            )
        replacements[node.output[0]] = node.input[0]
    if not replacements:
        model.CopyFrom(inferred)
        return 0

    def resolve(name: str) -> str:
        visited = set()
        while name in replacements:
            if name in visited:
                raise RuntimeError("Scale=0.5 Cast normalization found a cycle")
            visited.add(name)
            name = replacements[name]
        return name

    for node in kept_nodes:
        for index, name in enumerate(node.input):
            node.input[index] = resolve(name)
    inferred.graph.ClearField("node")
    inferred.graph.node.extend(kept_nodes)
    kept_value_info = [
        value
        for value in inferred.graph.value_info
        if value.name not in replacements
    ]
    inferred.graph.ClearField("value_info")
    inferred.graph.value_info.extend(kept_value_info)
    onnx.checker.check_model(inferred)
    model.CopyFrom(inferred)
    return len(replacements)


def inferred_value_types(model: onnx.ModelProto) -> dict[str, int]:
    inferred = onnx.shape_inference.infer_shapes(model)
    value_types = {
        value.name: value.type.tensor_type.elem_type
        for value in (
            *inferred.graph.input,
            *inferred.graph.output,
            *inferred.graph.value_info,
        )
    }
    value_types.update(
        {value.name: value.data_type for value in inferred.graph.initializer}
    )
    return value_types


def collect_mixed_precision_nodes(
    model: onnx.ModelProto,
) -> tuple[frozenset[str], int]:
    node_names = [node.name for node in model.graph.node]
    if any(not name for name in node_names) or len(node_names) != len(set(node_names)):
        raise RuntimeError("Scale=0.5 ONNX nodes must have unique non-empty names")

    producers: dict[str, onnx.NodeProto] = {}
    consumers: dict[str, list[onnx.NodeProto]] = {}
    for node in model.graph.node:
        for output in node.output:
            if not output:
                continue
            if output in producers:
                raise RuntimeError(f"Scale=0.5 tensor has two producers: {output}")
            producers[output] = node
        for input_name in node.input:
            if input_name:
                consumers.setdefault(input_name, []).append(node)

    selected: set[str] = set()

    def select_ancestors(tensor_name: str, stop_at_grid_sample: bool = False) -> None:
        node = producers.get(tensor_name)
        if (
            node is None
            or node.name in selected
            or node.op_type not in PRECISION_TRACE_OPERATORS
        ):
            return
        selected.add(node.name)
        if stop_at_grid_sample and node.op_type == "GridSample":
            return
        for input_name in node.input:
            select_ancestors(input_name, stop_at_grid_sample)

    grid_samples = [node for node in model.graph.node if node.op_type == "GridSample"]
    if len(grid_samples) != EXPECTED_GRID_SAMPLE_COUNT:
        raise RuntimeError(
            "Scale=0.5 graph must contain exactly five batched GridSample nodes, "
            f"got {len(grid_samples)}"
        )
    for node in grid_samples:
        if len(node.input) != 2 or len(node.output) != 1:
            raise RuntimeError(f"Unsupported GridSample contract: {node.name}")
        selected.add(node.name)
        select_ancestors(node.input[1])

    if len(model.graph.output) != 1:
        raise RuntimeError("Scale=0.5 graph must have exactly one output")
    select_ancestors(model.graph.output[0].name, stop_at_grid_sample=True)

    encoder_convolutions = []
    encoder_activations = []
    for node in model.graph.node:
        if (
            node.op_type != "Conv"
            or len(node.input) < 2
            or node.input[1] not in ENCODER_FP32_WEIGHTS
        ):
            continue
        if len(node.output) != 1:
            raise RuntimeError(f"Unsupported encoder Conv contract: {node.name}")
        output_consumers = consumers.get(node.output[0], [])
        if len(output_consumers) != 1 or output_consumers[0].op_type != "LeakyRelu":
            raise RuntimeError(
                f"Encoder Conv does not have one LeakyRelu consumer: {node.name}"
            )
        encoder_convolutions.append(node)
        encoder_activations.append(output_consumers[0])
        selected.add(node.name)
        selected.add(output_consumers[0].name)

    if (
        len(encoder_convolutions) != EXPECTED_FP32_CONVOLUTION_COUNT
        or len(encoder_activations) != EXPECTED_FP32_CONVOLUTION_COUNT
    ):
        raise RuntimeError(
            "Scale=0.5 graph does not contain both cnn0/cnn2 encoder branches: "
            f"conv={len(encoder_convolutions)}, "
            f"activation={len(encoder_activations)}"
        )

    selected_nodes = [node for node in model.graph.node if node.name in selected]
    operator_counts = Counter(node.op_type for node in selected_nodes)
    if (
        len(selected) != EXPECTED_PRECISION_NODE_COUNT
        or operator_counts != EXPECTED_PRECISION_OPERATOR_COUNTS
    ):
        raise RuntimeError(
            "Scale=0.5 precision graph structure changed: "
            f"expected_nodes={EXPECTED_PRECISION_NODE_COUNT}, "
            f"actual_nodes={len(selected)}, "
            f"expected_operators={dict(EXPECTED_PRECISION_OPERATOR_COUNTS)}, "
            f"actual_operators={dict(operator_counts)}"
        )

    value_types = inferred_value_types(model)
    fp32_operator_count = sum(
        any(value_types.get(output) == onnx.TensorProto.FLOAT for output in node.output)
        for node in selected_nodes
    )
    if fp32_operator_count != EXPECTED_FP32_OPERATOR_COUNT:
        raise RuntimeError(
            "Scale=0.5 precision graph has an unexpected FP32 operator count: "
            f"expected={EXPECTED_FP32_OPERATOR_COUNT}, "
            f"actual={fp32_operator_count}"
        )
    return frozenset(selected), fp32_operator_count


def verify_mixed_precision_graph(
    model: onnx.ModelProto,
    precision_node_names: frozenset[str],
) -> tuple[int, int]:
    require_fp16_contract(model, EXPECTED_FP32_INITIALIZERS)
    value_types = inferred_value_types(model)
    original_nodes = {
        node.name: node for node in model.graph.node if node.name in precision_node_names
    }
    missing = precision_node_names - original_nodes.keys()
    if missing:
        raise RuntimeError(
            "Scale=0.5 mixed conversion removed precision nodes: "
            f"{sorted(missing)[:8]}"
        )

    fp32_operator_count = sum(
        any(value_types.get(output) == onnx.TensorProto.FLOAT for output in node.output)
        for node in original_nodes.values()
    )
    if fp32_operator_count != EXPECTED_FP32_OPERATOR_COUNT:
        raise RuntimeError(
            "Scale=0.5 mixed conversion did not preserve every FP32 precision node: "
            f"expected={EXPECTED_FP32_OPERATOR_COUNT}, "
            f"actual={fp32_operator_count}"
        )

    fp32_convolutions = []
    for node in model.graph.node:
        if node.op_type not in ("Conv", "ConvTranspose"):
            continue
        output_types = {value_types.get(output) for output in node.output}
        if None in output_types:
            raise RuntimeError(f"Could not infer convolution output type: {node.name}")
        if onnx.TensorProto.FLOAT in output_types:
            fp32_convolutions.append(node)
        elif output_types != {onnx.TensorProto.FLOAT16}:
            raise RuntimeError(
                f"Scale=0.5 convolution has an invalid output type: {node.name}"
            )

    if (
        len(fp32_convolutions) != EXPECTED_FP32_CONVOLUTION_COUNT
        or any(node.op_type != "Conv" for node in fp32_convolutions)
        or {node.input[1] for node in fp32_convolutions} != ENCODER_FP32_WEIGHTS
    ):
        raise RuntimeError(
            "Scale=0.5 conversion preserved the wrong FP32 convolutions: "
            f"{[(node.name, node.op_type) for node in fp32_convolutions]}"
        )

    grid_samples = [node for node in model.graph.node if node.op_type == "GridSample"]
    if len(grid_samples) != EXPECTED_GRID_SAMPLE_COUNT or any(
        value_types.get(node.output[0]) != onnx.TensorProto.FLOAT
        for node in grid_samples
    ):
        raise RuntimeError("Scale=0.5 GridSample precision contract is invalid")
    return fp32_operator_count, len(fp32_convolutions)


def verify_dynamic_onnx_numerics(
    source_path: Path,
    mixed_path: Path,
) -> tuple[float, float]:
    source = ort.InferenceSession(str(source_path), providers=["CPUExecutionProvider"])
    mixed = ort.InferenceSession(str(mixed_path), providers=["CPUExecutionProvider"])
    max_error = 0.0
    error_sum = 0.0
    error_count = 0
    for index, (height, width, timestep) in enumerate(DYNAMIC_VALIDATION_CASES):
        values = validation_input(20260801 + index, timestep, height, width)
        contract = values.astype(np.float16)
        expected = source.run(None, {"input": contract.astype(np.float32)})[0]
        actual = mixed.run(None, {"input": contract})[0]
        if actual.shape != (1, 3, height, width):
            raise RuntimeError(
                "Mixed ONNX dynamic output shape is invalid: "
                f"expected={(1, 3, height, width)}, actual={actual.shape}"
            )
        difference = np.abs(actual.astype(np.float32) - expected.astype(np.float32))
        max_error = max(max_error, float(difference.max()))
        error_sum += float(difference.sum(dtype=np.float64))
        error_count += difference.size
    mean_error = error_sum / error_count
    if max_error > 2.0e-2 or mean_error > 1.0e-3:
        raise RuntimeError(
            "Dynamic FP32 ONNX to mixed ONNX validation failed: "
            f"max_abs={max_error:.8f}, mean_abs={mean_error:.8f}, "
            "max_limit=0.02000000, mean_limit=0.00100000"
        )
    return max_error, mean_error


def verify_export(
    reference: nn.Module,
    output_path: Path,
    input_dtype: np.dtype,
    stage: str,
    max_limit: float,
    mean_limit: float,
) -> tuple[float, float]:
    onnx_model = onnx.load(str(output_path))
    onnx.checker.check_model(onnx_model)
    session = ort.InferenceSession(str(output_path), providers=["CPUExecutionProvider"])
    max_error = 0.0
    error_sum = 0.0
    error_count = 0
    for index, timestep in enumerate((0.25, 0.5, 0.75)):
        input_tensor = make_validation_input(20260731 + index, timestep)
        contract_values = input_tensor.numpy().astype(input_dtype)
        reference_input = torch.from_numpy(contract_values.astype(np.float32))
        with torch.inference_mode():
            expected = reference(reference_input).cpu().numpy()
        actual = session.run(
            None, {"input": contract_values}
        )[0]
        if actual.shape != (1, 3, VALIDATION_SIZE, VALIDATION_SIZE):
            raise RuntimeError(f"ONNX output shape is invalid: {actual.shape}")
        difference = np.abs(actual - expected)
        max_error = max(max_error, float(difference.max()))
        error_sum += float(difference.sum(dtype=np.float64))
        error_count += difference.size
    mean_error = error_sum / error_count
    if max_error > max_limit or mean_error > mean_limit:
        raise RuntimeError(
            f"{stage} validation failed: max_abs={max_error:.8f}, "
            f"mean_abs={mean_error:.8f}, max_limit={max_limit:.8f}, "
            f"mean_limit={mean_limit:.8f}"
        )
    return max_error, mean_error


def add_metadata(
    model: onnx.ModelProto,
    weight_path: Path,
    source_sha256: str,
    fp32_operator_count: int,
    fp32_convolution_count: int,
) -> None:
    set_metadata(
        model,
        {
            "mediastation_model_id": MODEL_ID,
            "mediastation_scale": str(SCALE),
            "mediastation_precision": "fp16_io_mixed_fp32_islands",
            "mediastation_fp32_paths": (
                "encoder_cnn0_cnn2,flow_coordinates,grid_sample,output_blend"
            ),
            "mediastation_fp32_operator_count": str(fp32_operator_count),
            "mediastation_fp32_convolution_count": str(fp32_convolution_count),
            "mediastation_fp32_grid_sample_count": str(EXPECTED_GRID_SAMPLE_COUNT),
            "mediastation_input_contract": "fp16[1,11,H,W]",
            "mediastation_output_contract": "fp16[1,3,H,W]",
            "mediastation_engine_shape": "dynamic",
            "mediastation_shape_alignment": "128",
            "vs_rife_commit": UPSTREAM_COMMIT,
            "vs_rife_weight_sha256": sha256(weight_path),
            "mediastation_source_sha256": source_sha256,
            "export_tool": "rife_v426_scale05_export.py",
        },
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--vs-rife-dir", type=Path, required=True)
    parser.add_argument("--weight", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if not args.weight.is_file():
        raise FileNotFoundError(f"Official v4.26 weight is missing: {args.weight}")
    actual_weight_sha256 = sha256(args.weight)
    if actual_weight_sha256 != UPSTREAM_WEIGHT_SHA256:
        raise RuntimeError(
            "Official v4.26 weight SHA-256 mismatch: "
            f"expected={UPSTREAM_WEIGHT_SHA256}, actual={actual_weight_sha256}"
        )
    args.output.parent.mkdir(parents=True, exist_ok=True)

    model = load_upstream_ifnet(args.vs_rife_dir, args.weight)
    reference = OfficialRuntimeContractV426Scale05(model).eval()
    wrapper = RuntimeContractV426Scale05(model).eval()
    compatibility_max_error, compatibility_mean_error = (
        verify_pytorch_compatibility(reference, wrapper)
    )
    example = make_validation_input()
    temporary_fp32 = args.output.with_suffix(f"{args.output.suffix}.fp32.tmp")
    temporary_output = args.output.with_suffix(f"{args.output.suffix}.tmp")
    temporary_fp32.unlink(missing_ok=True)
    temporary_output.unlink(missing_ok=True)
    try:
        torch.onnx.export(
            wrapper,
            (example,),
            str(temporary_fp32),
            input_names=["input"],
            output_names=["output"],
            opset_version=16,
            do_constant_folding=True,
            dynamic_axes={
                "input": {2: "height", 3: "width"},
                "output": {2: "height", 3: "width"},
            },
            dynamo=False,
        )
        source_model = onnx.load(str(temporary_fp32))
        require_source_contract(source_model)
        removed_casts = remove_redundant_fp32_casts(source_model)
        onnx.save(source_model, str(temporary_fp32))
        fp32_max_error, fp32_mean_error = verify_export(
            reference,
            temporary_fp32,
            np.float32,
            "PyTorch to normalized FP32 ONNX",
            1.0e-5,
            1.0e-6,
        )
        source_sha256 = sha256(temporary_fp32)
        precision_node_names, source_fp32_operator_count = (
            collect_mixed_precision_nodes(source_model)
        )
        onnx_model = convert_float_to_float16(
            source_model,
            keep_io_types=False,
            node_block_list=sorted(precision_node_names),
        )
        fp32_operator_count, fp32_convolution_count = verify_mixed_precision_graph(
            onnx_model,
            precision_node_names,
        )
        if fp32_operator_count != source_fp32_operator_count:
            raise RuntimeError(
                "Scale=0.5 mixed conversion changed the FP32 precision node count: "
                f"source={source_fp32_operator_count}, "
                f"converted={fp32_operator_count}"
            )
        add_metadata(
            onnx_model,
            args.weight,
            source_sha256,
            fp32_operator_count,
            fp32_convolution_count,
        )
        onnx.checker.check_model(onnx_model)
        onnx.save(onnx_model, str(temporary_output))
        mixed_max_error, mixed_mean_error = verify_export(
            reference,
            temporary_output,
            np.float16,
            "PyTorch to mixed-precision ONNX",
            2.0e-2,
            1.0e-3,
        )
        dynamic_max_error, dynamic_mean_error = verify_dynamic_onnx_numerics(
            temporary_fp32,
            temporary_output,
        )
        temporary_output.replace(args.output)
    finally:
        temporary_fp32.unlink(missing_ok=True)
        temporary_output.unlink(missing_ok=True)
    output_sha256 = sha256(args.output)
    print(
        json.dumps(
            {
                "status": "RIFE_V426_SCALE05_ONNX_OK",
                "model_id": MODEL_ID,
                "scale": SCALE,
                "input": "1x11xHxW",
                "output": "1x3xHxW",
                "pytorch_to_fp32_onnx_max_abs": fp32_max_error,
                "pytorch_to_fp32_onnx_mean_abs": fp32_mean_error,
                "pytorch_to_mixed_onnx_max_abs": mixed_max_error,
                "pytorch_to_mixed_onnx_mean_abs": mixed_mean_error,
                "dynamic_fp32_to_mixed_onnx_max_abs": dynamic_max_error,
                "dynamic_fp32_to_mixed_onnx_mean_abs": dynamic_mean_error,
                "official_to_batched_pytorch_max_abs": compatibility_max_error,
                "official_to_batched_pytorch_mean_abs": compatibility_mean_error,
                "removed_redundant_fp32_casts": removed_casts,
                "fp32_precision_node_count": fp32_operator_count,
                "fp32_convolution_count": fp32_convolution_count,
                "fp32_grid_sample_count": EXPECTED_GRID_SAMPLE_COUNT,
                "weight_sha256": actual_weight_sha256,
                "upstream_commit": UPSTREAM_COMMIT,
                "output_path": str(args.output),
                "output_sha256": output_sha256,
            },
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()
