"""Export the official vs-rife v4.26 model at scale=0.5.

This is an export-time tool only. The generated graph keeps the native
rife_runtime.dll contract: FP16 [1, 11, H, W] -> [1, 3, H, W].
"""

from __future__ import annotations

import argparse
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
from torch import nn


UPSTREAM_COMMIT = "3488617283db7c428a83ba4a19382285da698b6a"
UPSTREAM_WEIGHT_SHA256 = "45c7f74156704769dc9f85cfcaf8552e1e926f9399dcfa3a553dee88fac6f53f"
UPSTREAM_IFNET_SHA256 = "0326ce02552c1c425517fdb2a6e9ffb23f174d092b0178fd9998f3d5036e607d"
UPSTREAM_WARP_SHA256 = "c3e47da7e968aa71c81cf2ce23c709e157cb5e07eab0f2de49df2a28aa2b1a14"
MODEL_ID = "rife-v4.26-scale0.5"
SCALE = 0.5
CHANNELS = 11
VALIDATION_SIZE = 128


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


class RuntimeContractV426Scale05(nn.Module):
    """Adapt the native 11-plane input to vs-rife's PyTorch call signature."""

    def __init__(self, model: nn.Module) -> None:
        super().__init__()
        self.model = model

    def forward(self, contract: torch.Tensor) -> torch.Tensor:
        values = contract.to(dtype=torch.float32)
        image0 = values[:, 0:3]
        image1 = values[:, 3:6]
        timestep = values[:, 6:7]
        grid = values[:, 7:9]
        flow_div = torch.stack(
            (1.0 / values[0, 9, 0, 0], 1.0 / values[0, 10, 0, 0])
        )
        feature0 = self.model.encode(image0)
        feature1 = self.model.encode(image1)
        output = self.model(
            image0,
            image1,
            timestep,
            flow_div,
            grid,
            feature0,
            feature1,
        )
        return output.to(dtype=torch.float16)


def make_validation_input(seed: int = 20260731, timestep: float = 0.5) -> torch.Tensor:
    torch.manual_seed(seed)
    values = torch.rand((1, CHANNELS, VALIDATION_SIZE, VALIDATION_SIZE), dtype=torch.float32)
    values[:, 6:7].fill_(timestep)
    horizontal = torch.linspace(-1.0, 1.0, VALIDATION_SIZE)
    vertical = torch.linspace(-1.0, 1.0, VALIDATION_SIZE)
    values[:, 7:8] = horizontal.view(1, 1, 1, -1)
    values[:, 8:9] = vertical.view(1, 1, -1, 1)
    values[:, 9:10].fill_(2.0 / (VALIDATION_SIZE - 1))
    values[:, 10:11].fill_(2.0 / (VALIDATION_SIZE - 1))
    return values.to(dtype=torch.float16)


def verify_export(wrapper: nn.Module, output_path: Path) -> tuple[float, float]:
    onnx_model = onnx.load(str(output_path))
    onnx.checker.check_model(onnx_model)
    session = ort.InferenceSession(str(output_path), providers=["CPUExecutionProvider"])
    max_error = 0.0
    error_sum = 0.0
    error_count = 0
    for index, timestep in enumerate((0.25, 0.5, 0.75)):
        input_tensor = make_validation_input(20260731 + index, timestep)
        with torch.inference_mode():
            reference = wrapper(input_tensor).cpu().numpy()
        actual = session.run(None, {"input": input_tensor.numpy()})[0]
        if actual.shape != (1, 3, VALIDATION_SIZE, VALIDATION_SIZE):
            raise RuntimeError(f"ONNX output shape is invalid: {actual.shape}")
        difference = np.abs(actual - reference)
        max_error = max(max_error, float(difference.max()))
        error_sum += float(difference.sum(dtype=np.float64))
        error_count += difference.size
    mean_error = error_sum / error_count
    max_limit, mean_limit = 1.0e-2, 1.0e-3
    if max_error > max_limit or mean_error > mean_limit:
        raise RuntimeError(
            f"PyTorch to ONNX validation failed: max_abs={max_error:.8f}, "
            f"mean_abs={mean_error:.8f}, max_limit={max_limit:.8f}, "
            f"mean_limit={mean_limit:.8f}"
        )
    return max_error, mean_error


def add_metadata(output_path: Path, weight_path: Path) -> None:
    model = onnx.load(str(output_path))
    metadata = {
        "mediastation_model_id": MODEL_ID,
        "mediastation_scale": str(SCALE),
        "mediastation_precision": "fp16_io",
        "mediastation_input_contract": "fp16[1,11,H,W]",
        "mediastation_output_contract": "fp16[1,3,H,W]",
        "mediastation_engine_shape": "dynamic",
        "mediastation_shape_alignment": "128",
        "vs_rife_commit": UPSTREAM_COMMIT,
        "vs_rife_weight_sha256": sha256(weight_path),
        "export_tool": "rife_v426_scale05_export.py",
    }
    model.metadata_props.clear()
    for key, value in metadata.items():
        entry = model.metadata_props.add()
        entry.key = key
        entry.value = value
    onnx.save(model, str(output_path))


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
    wrapper = RuntimeContractV426Scale05(model).eval()
    example = make_validation_input()
    temporary_output = args.output.with_suffix(f"{args.output.suffix}.tmp")
    temporary_output.unlink(missing_ok=True)
    try:
        torch.onnx.export(
            wrapper,
            (example,),
            str(temporary_output),
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
        max_error, mean_error = verify_export(wrapper, temporary_output)
        add_metadata(temporary_output, args.weight)
        onnx_model = onnx.load(str(temporary_output))
        io_values = (*onnx_model.graph.input, *onnx_model.graph.output)
        io_types = [value.type.tensor_type.elem_type for value in io_values]
        io_shapes = [
            tuple(
                dimension.dim_value or dimension.dim_param
                for dimension in value.type.tensor_type.shape.dim
            )
            for value in io_values
        ]
        if io_types != [onnx.TensorProto.FLOAT16, onnx.TensorProto.FLOAT16]:
            raise RuntimeError(f"ONNX IO contract is not FP16: {io_types}")
        if io_shapes != [(1, CHANNELS, "height", "width"), (1, 3, "height", "width")]:
            raise RuntimeError(f"ONNX IO shape contract is not dynamic: {io_shapes}")
        temporary_output.replace(args.output)
    finally:
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
                "pytorch_to_onnx_max_abs": max_error,
                "pytorch_to_onnx_mean_abs": mean_error,
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
