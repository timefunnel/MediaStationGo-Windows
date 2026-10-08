"""Compare the internal SDK CLI and owned builder, never user Engine caches.

Run with the development Python + numpy. Each --live-sequence is
name,media-id,start-seconds,bt709|bt2020nc. Uses the player's active Windows
credential only in memory; no playback reporting or preference writes.
"""

import argparse
import csv
import ctypes
from ctypes import wintypes
import hashlib
import json
import math
import os
from pathlib import Path
import re
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

import numpy as np


ROOT = Path(__file__).resolve().parents[2]
USER_AGENT = "MediaStationGoWindows/0.1.0-dev"
SCENE_CONTRACT = ("pair", "source0_pts", "source1_pts", "midpoint_pts", "class", "policy",
                  "average_delta", "changed_ratio", "average_kl", "regional_kl_max",
                  "chroma_delta", "edge_delta", "exposure_delta", "exposure_spread")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def run(arguments, environment, log=None, sensitive=False, timeout=300):
    try:
        result = subprocess.run(
            [str(arg) for arg in arguments], cwd=ROOT, env=environment,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        # TimeoutExpired embeds the command line, including signed media URLs.
        raise RuntimeError(f"{Path(arguments[0]).name} timed out after {timeout} seconds") from None
    output = result.stdout.decode("utf-8", errors="replace")
    if sensitive:
        output = re.sub(r"https?://[^\s\"']+", "[media-url-redacted]", output)
    if log is not None:
        log.write_text(output, encoding="utf-8")
    if result.returncode:
        raise RuntimeError(f"{Path(arguments[0]).name} failed ({result.returncode}): {output[-2500:]}")
    return output


def active_session():
    class Credential(ctypes.Structure):
        _fields_ = [
            ("flags", wintypes.DWORD), ("type", wintypes.DWORD),
            ("target", wintypes.LPWSTR), ("comment", wintypes.LPWSTR),
            ("written", wintypes.FILETIME), ("size", wintypes.DWORD),
            ("blob", ctypes.POINTER(ctypes.c_ubyte)),
            ("persist", wintypes.DWORD), ("count", wintypes.DWORD),
            ("attributes", wintypes.LPVOID), ("alias", wintypes.LPWSTR),
            ("user", wintypes.LPWSTR),
        ]

    api = ctypes.WinDLL("advapi32", use_last_error=True)
    api.CredReadW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                             ctypes.POINTER(ctypes.POINTER(Credential))]
    api.CredReadW.restype = wintypes.BOOL
    api.CredFree.argtypes = [wintypes.LPVOID]
    pointer = ctypes.POINTER(Credential)()
    if not api.CredReadW("MediaStationGo.Windows.ActiveSession.v1", 1, 0, ctypes.byref(pointer)):
        raise RuntimeError(f"Active player credential unavailable: {ctypes.get_last_error()}")
    try:
        return json.loads(ctypes.string_at(pointer.contents.blob, pointer.contents.size))
    finally:
        api.CredFree(pointer)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        return None


def origin(url):
    parsed = urllib.parse.urlsplit(url)
    return parsed.scheme, parsed.hostname, parsed.port or (443 if parsed.scheme == "https" else 80)


def acquire_sequence(specification, directory, ffmpeg, environment, frames):
    name, media_id, start, matrix = specification.split(",")
    if not re.fullmatch(r"[a-z0-9-]+", name) or not media_id.isdigit():
        raise ValueError("Sequence name or media id is invalid")
    if matrix not in ("bt709", "bt2020nc") or float(start) < 0:
        raise ValueError("Sequence matrix or start time is invalid")
    session = active_session()
    base = session["baseUrl"].rstrip("/") + "/"
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    headers = {"User-Agent": USER_AGENT, "X-Emby-Token": session["accessToken"]}
    url = base + f"Items/{media_id}/PlaybackInfo?" + urllib.parse.urlencode({
        "UserId": session["userId"], "AutoOpenLiveStream": "false", "IsPlayback": "false",
    })
    with opener.open(urllib.request.Request(url, headers=headers), timeout=25) as response:
        sources = json.load(response)["MediaSources"]
    if len(sources) != 1:
        raise RuntimeError("Validation requires one explicit media source; select a version first")
    source = sources[0]
    if source.get("DirectStreamUrl"):
        url = urllib.parse.urljoin(base, source["DirectStreamUrl"])
    else:
        # This is the existing player's standard Emby endpoint contract,
        # not a retry/fallback after a failed DirectStreamUrl.
        url = base + f"Videos/{media_id}/stream?" + urllib.parse.urlencode({
            "Container": source["Container"], "Static": "true", "MediaSourceId": source["Id"],
        })
    if origin(url) != origin(base):
        raise RuntimeError("Initial playback URL must share the media server origin")
    parsed = urllib.parse.urlsplit(url)
    query = [(key, value) for key, value in urllib.parse.parse_qsl(parsed.query)
             if key.lower() not in ("api_key", "apikey", "access_token", "token", "x-emby-token")]
    url = urllib.parse.urlunsplit(parsed._replace(query=urllib.parse.urlencode(query)))
    # Resolve using the player's same-UA Range policy. Never forward the server
    # access token to a CDN, and never log signed URLs.
    redirects = 0
    while True:
        parsed = urllib.parse.urlsplit(url)
        if parsed.scheme not in ("http", "https") or parsed.username or parsed.password:
            raise RuntimeError("Invalid validation stream URL")
        request_headers = {"User-Agent": USER_AGENT, "Range": "bytes=0-0"}
        if origin(url) == origin(base):
            request_headers["X-Emby-Token"] = session["accessToken"]
        try:
            response = opener.open(urllib.request.Request(url, headers=request_headers), timeout=25)
        except urllib.error.HTTPError as error:
            if error.code not in (301, 302, 303, 307, 308):
                raise RuntimeError(f"Stream Range probe HTTP {error.code}") from None
            location = error.headers.get("Location")
            error.close()
            if not location or redirects >= 6:
                raise RuntimeError("Stream redirect chain invalid or too long")
            next_url = urllib.parse.urljoin(url, location)
            if parsed.scheme == "https" and urllib.parse.urlsplit(next_url).scheme != "https":
                raise RuntimeError("Stream redirect attempted an HTTPS downgrade")
            url = next_url
            redirects += 1
            continue
        with response:
            if response.status != 206 or not response.headers.get("Content-Range"):
                raise RuntimeError("Validation stream does not support byte Range")
            response.read(1)
        break
    if origin(url) == origin(base):
        raise RuntimeError("Expected resolved CDN URL; do not pass server credentials to ffmpeg")
    ffprobe = ffmpeg.with_name("ffprobe.exe")
    metadata = json.loads(run([
        ffprobe, "-v", "error", "-rw_timeout", "25000000", "-user_agent", USER_AGENT,
        "-select_streams", "v:0", "-show_entries",
        "stream=width,height,r_frame_rate,color_space,color_transfer,color_primaries,color_range,pix_fmt",
        "-of", "json", url,
    ], environment, sensitive=True, timeout=90))["streams"][0]
    if metadata.get("color_space") != matrix or metadata.get("color_range") != "tv":
        raise RuntimeError(f"Unexpected source color contract: {metadata}")
    if matrix == "bt2020nc" and metadata.get("color_transfer") != "smpte2084":
        raise RuntimeError("HDR test source is not HDR10/PQ")
    if matrix == "bt709" and metadata.get("color_transfer") != "bt709":
        raise RuntimeError("SDR test source is not BT.709")
    width, height = metadata["width"], metadata["height"]
    target = directory / f"{name}.p010"
    run([
        ffmpeg, "-hide_banner", "-v", "error", "-nostdin", "-rw_timeout", "25000000",
        "-user_agent", USER_AGENT, "-ss", start, "-i", url, "-map", "0:v:0",
        "-frames:v", frames, "-an", "-sn", "-fps_mode", "passthrough",
        "-pix_fmt", "p010le", "-f", "rawvideo", target,
    ], environment, sensitive=True, timeout=180)
    if target.stat().st_size != width * height * 3 * frames:
        raise RuntimeError("Extracted P010 sequence has an unexpected length")
    result = dict(name=name, media_id=media_id, start=float(start), matrix=matrix,
                  width=width, height=height, fps=metadata["r_frame_rate"], frames=frames,
                  source_sha256=digest(target), metadata=metadata, redirects=redirects)
    print("RIFE_AB_SOURCE " + json.dumps(result, ensure_ascii=False), flush=True)
    return result, target


def builder_arguments(model, onnx, engine):
    arguments = [f"--onnx={onnx.relative_to(ROOT)}", f"--saveEngine={engine.relative_to(ROOT)}"]
    for index, profile in enumerate(model["profiles"]):
        arguments.append(f"--profile={index}")
        for selector in ("min", "opt", "max"):
            arguments.append(f"--{selector}Shapes=input:1x11x{profile[selector + 'Height']}x{profile[selector + 'Width']}")
    return arguments


def sequence_paths(prefix):
    # Model ids contain dots. Replacing a suffix would alias old/new outputs.
    return tuple(Path(str(prefix) + suffix) for suffix in (".p010", ".csv", ".log"))


def self_test():
    names = [sequence_paths(ROOT / f"{model}-{source}-{builder}")
             for model in ("rife-v4.26", "rife-v4.26-scale0.5", "rife-v4.25-lite")
             for source in ("alita", "forrest") for builder in ("old", "new")]
    if len({path for group in names for path in group}) != 36:
        raise RuntimeError("A/B filenames collide")
    try:
        compare_sequence(ROOT / "same.p010", ROOT / "same.p010", ROOT / "a.csv", ROOT / "b.csv", 2, 2)
    except RuntimeError as error:
        if "separate files" not in str(error):
            raise
    else:
        raise RuntimeError("Aliased A/B paths were not rejected")
    with tempfile.TemporaryDirectory(prefix="rife-ab-self-test-", dir=ROOT / "build") as temporary:
        directory = Path(temporary)
        old, new = directory / "old.p010", directory / "new.p010"
        old_csv, new_csv = directory / "old.csv", directory / "new.csv"
        values = np.full(6, 100 << 6, dtype="<u2")
        old.write_bytes(values.tobytes())
        new.write_bytes(values.tobytes())
        row = {key: "0" for key in SCENE_CONTRACT}
        row["policy"] = "rife-midpoint"
        for path in (old_csv, new_csv):
            with path.open("w", encoding="utf-8", newline="") as destination:
                writer = csv.DictWriter(destination, fieldnames=SCENE_CONTRACT)
                writer.writeheader()
                writer.writerow(row)
        result = compare_sequence(old, new, old_csv, new_csv, 2, 2)
        if result["exact_frames"] != 1 or result["mae_10bit"] != 0:
            raise RuntimeError("Exact-output comparison failed")
        values[0] += 64
        new.write_bytes(values.tobytes())
        result = compare_sequence(old, new, old_csv, new_csv, 2, 2)
        if result["exact_frames"] != 0 or result["max_10bit"] != 1 or result["changed_ratio"] != 1 / 6:
            raise RuntimeError("One-code P010 difference was not detected")
        row["midpoint_pts"] = "0.1"
        with new_csv.open("w", encoding="utf-8", newline="") as destination:
            writer = csv.DictWriter(destination, fieldnames=SCENE_CONTRACT)
            writer.writeheader()
            writer.writerow(row)
        try:
            compare_sequence(old, new, old_csv, new_csv, 2, 2)
        except RuntimeError as error:
            if "PTS changed" not in str(error):
                raise
        else:
            raise RuntimeError("PTS changes were not rejected")
    print("RIFE_AB_SELF_TEST_OK unique_paths=36 alias_rejection=yes exact=yes pixel_difference=yes pts_rejection=yes", flush=True)


def compare_sequence(old, new, old_csv, new_csv, width, height):
    if old.resolve() == new.resolve() or old_csv.resolve() == new_csv.resolve():
        raise RuntimeError("A/B outputs must be separate files")
    with old_csv.open(encoding="utf-8") as source:
        old_rows = list(csv.DictReader(source))
    with new_csv.open(encoding="utf-8") as source:
        new_rows = list(csv.DictReader(source))
    if len(old_rows) != len(new_rows) or not old_rows:
        raise RuntimeError("Sequence diagnostic lengths do not match")
    for left, right in zip(old_rows, new_rows):
        if any(left[key] != right[key] for key in SCENE_CONTRACT):
            raise RuntimeError("Scene classification, policy or PTS changed")
    frame_samples = width * height * 3 // 2
    expected = frame_samples * 2 * len(old_rows)
    if old.stat().st_size != expected or new.stat().st_size != expected:
        raise RuntimeError("Sequence output length changed")
    total_abs = total_squared = changed = maximum = exact = 0
    with old.open("rb") as left, new.open("rb") as right:
        for _ in old_rows:
            a = np.frombuffer(left.read(frame_samples * 2), dtype="<u2")
            b = np.frombuffer(right.read(frame_samples * 2), dtype="<u2")
            if np.any((a | b) & 63):
                raise RuntimeError("Output is not packed 10-bit P010")
            exact += int(np.array_equal(a, b))
            difference = (a >> 6).astype(np.int32) - (b >> 6).astype(np.int32)
            total_abs += int(np.abs(difference).sum(dtype=np.int64))
            total_squared += int(np.square(difference).sum(dtype=np.int64))
            changed += int(np.count_nonzero(difference))
            maximum = max(maximum, int(np.abs(difference).max()))
    samples = frame_samples * len(old_rows)
    mse = total_squared / samples
    return dict(midpoints=len(old_rows), exact_frames=exact, mae_10bit=total_abs / samples,
                max_10bit=maximum, changed_ratio=changed / samples,
                psnr_db=None if mse == 0 else 10 * math.log10(1023 ** 2 / mse),
                scene_pts_policy_equal=True, cuts=sum(row["policy"] == "copy-f0-hard-cut" for row in old_rows),
                old_sha256=digest(old), new_sha256=digest(new))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live-sequence", action="append")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--ffmpeg", type=Path, default=Path(r"C:\msys64\clang64\bin\ffmpeg.exe"))
    parser.add_argument("--frames", type=int, default=24)
    parser.add_argument("--warmup", type=int, default=30)
    parser.add_argument("--iterations", type=int, default=100)
    parser.add_argument("--pairs", type=int, default=3)
    arguments = parser.parse_args()
    if arguments.self_test:
        self_test()
        return
    if not arguments.live_sequence:
        parser.error("At least one --live-sequence is required")
    self_test()
    if not 2 <= arguments.frames <= 240 or min(arguments.warmup, arguments.iterations, arguments.pairs) < 1:
        parser.error("Invalid validation sample counts")
    runtime_lib = ROOT / "third_party/mpv-install/lib"
    development = ROOT / "third_party/frame-interpolation-runtime"
    sdk = development / ".tensorrt/TensorRT-RTX-1.4.0.76/bin"
    owned = ROOT / "build/rife-runtime/rife_engine_builder.exe"
    reference = sdk / "tensorrt_rtx.exe"
    probe = ROOT / "build/rife-runtime/rife_runtime_probe.exe"
    runtime = runtime_lib / "rife_runtime.dll"
    cuda = runtime_lib / "cudart64_12.dll"
    environment = dict(os.environ)
    environment["PATH"] = os.pathsep.join(map(str, [sdk, development / "bin", runtime_lib, arguments.ffmpeg.parent])) + os.pathsep + environment["PATH"]
    manifest = json.loads((runtime_lib / "frame-interpolation/runtime-manifest.json").read_text(encoding="utf-8-sig"))
    report = dict(owned_builder_sha256=digest(owned), reference_builder_sha256=digest(reference),
                  runtime_sha256=digest(runtime), warmup=arguments.warmup,
                  iterations=arguments.iterations, benchmark_pairs=arguments.pairs, models=[])
    with tempfile.TemporaryDirectory(prefix="rife-builder-ab-", dir=ROOT / "build") as temporary:
        directory = Path(temporary)
        sequences = [acquire_sequence(spec, directory, arguments.ffmpeg, environment, arguments.frames)
                     for spec in arguments.live_sequence]
        report["sources"] = [source for source, _ in sequences]
        for model in manifest["models"]:
            onnx = runtime_lib / "frame-interpolation/models" / model["onnxFile"]
            if digest(onnx) != model["onnxSha256"]:
                raise RuntimeError("Manifest ONNX hash mismatch")
            entry = dict(id=model["id"], onnx_sha256=model["onnxSha256"],
                         profiles=model["profiles"], alignment=model["shapeAlignment"], engines={}, sequences=[], benchmarks=[])
            engines = {}
            for kind, builder in (("old", reference), ("new", owned)):
                engine = directory / f"{model['id']}-{kind}.engine"
                options = builder_arguments(model, onnx, engine)
                if kind == "old":
                    options += ["--skipInference", "--useGpu"]
                started = time.monotonic()
                log = run([builder, *options], environment, directory / f"{model['id']}-{kind}-build.log")
                if kind == "new" and "avgTiming=8 optimizationLevel=3 profiling=layer_names_only" not in log:
                    raise RuntimeError("Owned builder configuration was not verified")
                entry["engines"][kind] = dict(sha256=digest(engine), bytes=engine.stat().st_size,
                                             build_seconds=time.monotonic() - started)
                engines[kind] = engine
                print(f"RIFE_AB_BUILT model={model['id']} builder={kind}", flush=True)
            environment["MSGO_RIFE_ALIGNMENT"] = str(model["shapeAlignment"])
            environment["MSGO_RIFE_PROFILE"] = "1"
            for source, source_path in sequences:
                environment["MSGO_RIFE_COLOR_MATRIX"] = source["matrix"]
                files = {}
                selected_profiles = {}
                numerator, denominator = source["fps"].split("/")
                for kind, engine in engines.items():
                    prefix = directory / f"{model['id']}-{source['name']}-{kind}"
                    output, diagnostics, sequence_log = sequence_paths(prefix)
                    log = run([probe, "--sequence", runtime, engine, cuda, source["width"], source["height"],
                               numerator, denominator, source["start"], source_path, output, diagnostics],
                              environment, sequence_log)
                    if "RIFE_SEQUENCE_OK" not in log or "failures=0" not in log:
                        raise RuntimeError("Native sequence probe did not complete")
                    selected_profiles[kind] = int(re.search(r"RIFE_SEQUENCE_OK profile=(\d+)", log)[1])
                    files[kind] = output, diagnostics
                comparison = compare_sequence(files["old"][0], files["new"][0], files["old"][1],
                                              files["new"][1], source["width"], source["height"])
                comparison["source"] = source["name"]
                comparison["selected_profiles"] = selected_profiles
                entry["sequences"].append(comparison)
                print("RIFE_AB_PIXELS " + json.dumps(dict(model=model["id"], **comparison)), flush=True)
            environment["MSGO_RIFE_COLOR_MATRIX"] = "bt709"
            measurements = {"old": [], "new": []}
            for pair in range(arguments.pairs):
                # Alternate order to limit one-way clock/temperature drift.
                for kind in (("old", "new") if pair % 2 == 0 else ("new", "old")):
                    log = run([probe, runtime, engines[kind], cuda, 3840, 2160,
                               arguments.warmup, arguments.iterations], environment,
                              directory / f"{model['id']}-{kind}-benchmark-{pair}.log")
                    summary = re.search(r"RIFE_RUNTIME_PROBE_OK .*", log)
                    if not summary or "p010-packed=yes" not in summary[0] or "cache-reopen=hit" not in summary[0]:
                        raise RuntimeError("Benchmark did not satisfy the native P010/cache contract")
                    metrics = re.search(r"mean=([0-9.]+)ms p95=([0-9.]+)ms", summary[0])
                    if not metrics:
                        raise RuntimeError("Benchmark measurements missing")
                    measurement = dict(builder=kind, pair=pair, mean_ms=float(metrics[1]), p95_ms=float(metrics[2]))
                    entry["benchmarks"].append(measurement)
                    measurements[kind].append(measurement["mean_ms"])
            medians = {kind: statistics.median(values) for kind, values in measurements.items()}
            entry["benchmark_medians_ms"] = medians
            entry["benchmark_change_percent"] = (medians["new"] / medians["old"] - 1) * 100
            report["models"].append(entry)
            print("RIFE_AB_PERFORMANCE " + json.dumps(dict(model=model["id"], medians_ms=medians,
                   change_percent=entry["benchmark_change_percent"])), flush=True)
    report["all_sampled_pixels_exact"] = all(
        sequence["exact_frames"] == sequence["midpoints"]
        for model in report["models"] for sequence in model["sequences"])
    target = ROOT / "build/rife-builder-equivalence.json"
    target.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"RIFE_AB_REPORT path={target} sampled_pixels_exact={report['all_sampled_pixels_exact']} temp_cleaned=yes", flush=True)
    if not report["all_sampled_pixels_exact"]:
        raise RuntimeError("Numerical differences found; explicit visual acceptance required, not an equivalence pass")


if __name__ == "__main__":
    main()
