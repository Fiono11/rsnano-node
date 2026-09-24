#!/usr/bin/env python3
"""Paired, sequential runs using explicit node and shared client binaries.

No retry or successful-run filtering. Each attempt retains its log, node data,
RPC snapshots, command, binary hashes and result. Run on an otherwise idle host.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import signal
import statistics
import subprocess
import time
import urllib.request


def sha256(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def quantile(histogram, fraction):
    rank = math.ceil(sum(histogram.values()) * fraction)
    count = 0
    for latency, n in sorted((int(k), n) for k, n in histogram.items()):
        count += n
        if count >= rank:
            return latency
    return None


def rpc(port, body):
    request = urllib.request.Request(
        f"http://[::1]:{port}", json.dumps(body).encode(),
        {"Content-Type": "application/json"},
    )
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=3) as response:
        return json.load(response)


def run(args, label, binary, pair):
    directory = args.out / f"pair-{pair:02d}-{label}"
    directory.mkdir()
    # Node launcher resolves rsnano via PATH; use a directory containing only
    # the exact binary selected for this attempt.
    bindir = directory / "bin"
    bindir.mkdir()
    (bindir / "rsnano").symlink_to(binary)
    data = directory / "data"
    data.mkdir()
    command = [str(args.client), "--data-dir", str(data), "--prs", "6",
               "--no-prio", "--blocks", str(args.blocks), "--accounts", str(args.accounts),
               "--rate", str(args.rate), "--fork-percentage", str(args.forks), "--no-kill"]
    if args.epoch_ms:
        command += ["--epoch-duration-ms", str(args.epoch_ms)]
    command += args.extra
    env = os.environ | {"PATH": str(bindir) + os.pathsep + os.environ["PATH"],
                        "RUST_LOG": "nanospam=info", "NANO_LOG": "noansi"}
    result = {"pair": pair, "label": label, "command": command,
              "node": str(binary), "node_sha256": sha256(binary),
              "client_sha256": sha256(args.client), "started": time.time()}
    snapshots = []
    with (directory / "run.log").open("w") as log:
        process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        try:
            try:
                result["exit_code"] = process.wait(timeout=args.timeout)
                result["timed_out"] = False
            except subprocess.TimeoutExpired:
                result.update(exit_code=None, timed_out=True)
            for i in range(6 - args.absent):
                snapshot = {"node": i}
                for action in ("block_count", "final_state", "stats"):
                    try:
                        body = {"action": action}
                        if action == "stats":
                            body["type"] = "counters"
                        snapshot[action] = rpc(17076 + 10 * i, body)
                    except Exception as error:
                        snapshot[action] = {"collection_error": str(error)}
                snapshots.append(snapshot)
        finally:
            # Only the process group created for this run; never global pkill.
            try:
                os.killpg(process.pid, signal.SIGTERM)
                time.sleep(1)
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
    result["wall_secs"] = time.time() - result["started"]
    text = (directory / "run.log").read_text(errors="replace")
    summaries = [line.split("RAI_BENCH_METRICS ", 1)[1]
                 for line in text.splitlines() if "RAI_BENCH_METRICS " in line]
    if summaries:
        metrics = json.loads(summaries[-1])
        result["metrics"] = metrics
        result["complete"] = (not result["timed_out"] and result["exit_code"] == 0
                              and metrics["confirmed"] == metrics["created"] == args.blocks)
        result["goodput"] = metrics["confirmed"] / metrics["duration_secs"]
        for percentile in (50, 95, 99):
            result[f"p{percentile}_ms"] = quantile(
                metrics["confirmation_histogram_ms"], percentile / 100)
    else:
        result["complete"] = False
    (directory / "rpc.json").write_text(json.dumps(snapshots, indent=2) + "\n")
    (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k != "metrics"}), flush=True)
    return result


def interval(values):
    # Paired percentile bootstrap, with a fixed analysis seed. Exploratory
    # at small n; this is a regression gate, not a universal performance claim.
    rng = random.Random(731)
    samples = sorted(statistics.mean(rng.choices(values, k=len(values))) for _ in range(10000))
    return [samples[250], samples[9749]]


def compare(results, repetitions):
    if not all(r["complete"] for r in results):
        return {"verdict": "FAIL_COMPLETION", "attempts": len(results)}
    if repetitions < 5:
        return {"verdict": "SMOKE_ONLY", "attempts": len(results)}
    pairs = [{r["label"]: r for r in results if r["pair"] == i} for i in range(repetitions)]
    goodput = interval([p["candidate"]["goodput"] / p["baseline"]["goodput"] for p in pairs])
    if any(p["baseline"]["p99_ms"] == 0 for p in pairs):
        return {"verdict": "INCONCLUSIVE_ZERO_LATENCY", "goodput_ratio_ci95": goodput}
    latency = interval([p["candidate"]["p99_ms"] / p["baseline"]["p99_ms"] for p in pairs])
    verdict = "PASS" if goodput[0] >= .95 and latency[1] <= 1.10 else "INCONCLUSIVE"
    if goodput[1] < .95 or latency[0] > 1.10:
        verdict = "REGRESSION"
    return {"verdict": verdict, "goodput_ratio_ci95": goodput, "p99_ratio_ci95": latency}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("baseline", "candidate", "client", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--blocks", type=int, default=45000)
    parser.add_argument("--accounts", type=int, default=45000)
    parser.add_argument("--rate", type=int, default=2000)
    parser.add_argument("--epoch-ms", type=int, default=8000)
    parser.add_argument("--forks", type=int, default=0)
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("--absent", type=int, default=0)
    parser.add_argument("extra", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.extra[:1] == ["--"]:
        args.extra.pop(0)
    for name in ("baseline", "candidate", "client", "out"):
        setattr(args, name, getattr(args, name).resolve())
    args.out.mkdir(parents=True, exist_ok=False)
    manifest = {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()}
    manifest.update(platform=platform.platform(), machine=platform.machine(),
                    harness_sha256=sha256(Path(__file__)),
                    workload_seed=None,
                    workload_note="Shared pinned client and parameters; HEAD generator is not seeded.")
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    results = []
    for pair in range(args.repetitions):
        order = [("baseline", args.baseline), ("candidate", args.candidate)]
        if pair % 2:
            order.reverse()
        for label, binary in order:
            results.append(run(args, label, binary, pair))
    verdict = compare(results, args.repetitions)
    (args.out / "comparison.json").write_text(json.dumps(verdict, indent=2) + "\n")
    print(json.dumps(verdict), flush=True)
    return 0 if verdict["verdict"] in ("PASS", "SMOKE_ONLY") else 1


if __name__ == "__main__":
    raise SystemExit(main())
