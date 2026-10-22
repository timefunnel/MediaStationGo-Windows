"""Compare diagnostic rendered pixels with VFG's retained inputs and outputs.

Requires numpy. All input/output stays local; no media requests are made.
"""

import argparse
import csv
import json
import re
from pathlib import Path

import numpy as np


def load_capture(path, encoding):
    lines = path.read_text(encoding="utf-8-sig").splitlines()
    if not lines or not re.fullmatch(
        r"# complete written=\d+ dropped=0 generated=\d+", lines[-1]
    ):
        raise ValueError(f"Incomplete capture or dropped pixels: {path.name}")
    if "thumbnail=320x180" not in lines[0] or f"encoding={encoding}" not in lines[0]:
        raise ValueError(f"Unsupported pixel encoding or thumbnail dimensions: {path.name}")
    frequency = int(re.search(r"frequency=(\d+)", lines[0]).group(1))
    rows = list(csv.DictReader(line for line in lines if not line.startswith("#")))
    rows.sort(key=lambda row: int(row["sequence"]))
    raw = np.memmap(path.with_suffix(".rgba"), dtype=np.uint8, mode="r")
    images = np.stack([
        raw[int(row["offset"]):int(row["offset"]) + 230400]
        .reshape(180, 320, 4)[..., :3] for row in rows
    ])
    return rows, images, frequency


def normalize(images):
    # Ignore subtitle/border areas; normalized luminance tolerates SDR output
    # conversion without treating near-identical neighboring frames as certain.
    values = images[:, 12:156:6, 6:312:6].astype(np.float32).mean(-1)
    values = values.reshape(len(images), -1)
    return (values - values.mean(1, keepdims=True)) / np.maximum(
        values.std(1, keepdims=True), 0.1
    )


def analyze(directory, source_fps, sdk_path=None, render_path=None):
    if sdk_path is None:
        sdk_path = max(directory.glob("session-*.csv"), key=lambda p: p.stat().st_mtime_ns)
        render_path = max(directory.glob("render-*.csv"), key=lambda p: p.stat().st_mtime_ns)
    source, source_images, frequency = load_capture(sdk_path, "sdr-rgba")
    rendered, rendered_images, render_frequency = load_capture(render_path, "display-rgba")
    if frequency != render_frequency:
        raise ValueError("Captures use different clock frequencies")
    period = 1 / source_fps
    first_pair = min(int(row["pair"]) for row in source)
    references = {}
    for row, image in zip(source, source_images):
        phase = {"previous": 0.0, "current": 1.0}.get(row["kind"])
        if phase is None:
            if row["kind"] != "generated":
                raise ValueError(f"Unexpected SDK record: {row['kind']}")
            phase = float(row["timestep"])
        time = round((int(row["pair"]) - first_pair + phase) * period, 9)
        references[time] = (row, image)
    times = np.array(sorted(references))
    source = [references[time][0] for time in times]
    source_images = np.stack([references[time][1] for time in times])
    a, b = normalize(source_images), normalize(rendered_images)
    scores = np.maximum(0, 2 - 2 * b @ a.T / a.shape[1])
    rqpc = np.array([int(row["qpc"]) for row in rendered])
    sqpc = np.array([int(row["qpc"]) for row in source])
    scores[np.abs(rqpc[:, None] - sqpc[None, :]) > frequency * 0.6] = np.inf
    best = scores.argmin(1)
    distances = scores.min(1)
    ordered = np.sort(scores, axis=1)
    gaps = np.zeros(len(rendered))
    finite = np.isfinite(ordered[:, 1])
    gaps[finite] = ordered[finite, 1] - ordered[finite, 0]
    pts = np.array([float(row["pts"]) for row in rendered])
    valid = np.array([int(row["index"]) == 1 for row in rendered])
    valid &= np.isfinite(distances) & (np.abs(pts) < 1e7)
    clear = valid & (distances < 0.15) & (gaps > 0.0005)
    if clear.sum() < 100:
        raise ValueError("Too few unambiguous pixel matches; capture continuous motion again")
    if np.any(np.diff(pts[valid]) < -1e-6):
        raise ValueError("Playback PTS moved backwards during capture; use continuous playback")
    # A median alignment requires a majority of correct matches. Record its
    # distribution so a widespread alignment failure cannot look conclusive.
    offsets = pts - times[best]
    offset = float(np.median(offsets[clear]))
    events = []
    for number, row in enumerate(rendered):
        if not valid[number]:
            continue
        relative = pts[number] - offset
        expected = int(np.abs(times - relative).argmin())
        if relative < times[0] + period or relative > times[-1] - period:
            continue
        if abs(times[expected] - relative) > 1e-5:
            continue
        events.append(dict(
            present=int(row["pair"]), pts=float(pts[number]),
            expected_seconds=float(times[expected]), matched_seconds=float(times[best[number]]),
            delta_ms=float((times[best[number]] - times[expected]) * 1000),
            error=float(distances[number]), gap=float(gaps[number]), clear=bool(clear[number])
        ))
    wrong = [event for event in events if event["clear"] and abs(event["delta_ms"]) > 0.01]
    certain = [event for event in events if event["clear"]]
    backwards = [dict(before=before, after=after)
                 for before, after in zip(certain, certain[1:])
                 if after["matched_seconds"] < before["matched_seconds"] - 1e-6]
    if len(certain) < 100:
        raise ValueError("Too few aligned rendered frames; verify source FPS and capture overlap")
    result = dict(
        sdk=sdk_path.name, rendered=render_path.name, source_fps=source_fps,
        rendered_frames=len(rendered), eligible_frames=len(events), clear_matches=len(certain),
        offset_seconds=offset, offset_percentiles=np.percentile(offsets[clear], [0, 10, 50, 90, 100]).tolist(),
        wrong_count=len(wrong), backward_count=len(backwards), wrong=wrong, backward=backwards,
        limitations="Before-Present SDR pixel comparison; ambiguous images excluded; majority alignment required"
    )
    output = render_path.with_suffix(".pixels.json")
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in result.items()
                      if key not in ("wrong", "backward")}, indent=2))
    return bool(wrong or backwards)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--source-fps", required=True, type=float)
    parser.add_argument("--sdk", type=Path, help="Explicit SDK CSV; requires --render")
    parser.add_argument("--render", type=Path, help="Explicit rendered CSV; requires --sdk")
    args = parser.parse_args()
    if (args.sdk is None) != (args.render is None):
        parser.error("provide both --sdk and --render")
    if not np.isfinite(args.source_fps) or args.source_fps <= 0:
        parser.error("source-fps must be positive and finite")
    raise SystemExit(analyze(args.directory, args.source_fps, args.sdk, args.render))
