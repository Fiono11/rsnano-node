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
import shutil
import socket
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


def settled(states, forks=0):
    """All PRs terminated with the same state: every node's final-state hash
    (its cemented frontiers) and cemented count are equal. Without forks
    every held block must also be cemented and no election pending, as
    before. With forks an exactly split position may stay unresolved, and
    elections still collecting votes on it are reported (`state_summary`),
    not required to vanish: they do not change the state."""
    if not states or any("count" not in s["block_count"] for s in states):
        return False
    if forks == 0 and any(s["block_count"].get("count") != s["block_count"].get("cemented")
                          or s["final_state"].get("pending") != "0" for s in states):
        return False
    return (len({s["final_state"].get("hash") for s in states}) == 1
            and len({s["block_count"].get("cemented") for s in states}) == 1)



def checkpoints_consistent(states, required):
    """Every required checkpoint is decided and installed with the same value."""
    if required == 0:
        return True
    if not states:
        return False
    for epoch in range(required):
        values = []
        for state in states:
            entries = state.get("final_state", {}).get("epochs", [])
            close = next((entry.get("close", {}) for entry in entries
                          if str(entry.get("epoch")) == str(epoch)), {})
            value, decided = close.get("value"), close.get("closed_value")
            if not value or not decided:
                return False
            values.append((value, decided))
        if len(set(values)) != 1:
            return False
    return True


def frontier_diff(frontiers):
    """Compare every node's confirmed (height, frontier) per account. Nodes
    at different heights are lagging; nodes at the same height with different
    frontiers cemented conflicting blocks, which is a safety violation."""
    accounts = set()
    for per_node in frontiers.values():
        accounts |= set(per_node)
    lagging, conflicting = [], []
    for account in sorted(accounts):
        seen = {node: per_node.get(account) for node, per_node in frontiers.items()}
        by_height = {}
        for node, value in seen.items():
            if value is not None:
                by_height.setdefault(value[0], set()).add(value[1])
        if any(len(hashes) > 1 for hashes in by_height.values()):
            conflicting.append({"account": account, "nodes": seen})
        elif len({v for v in seen.values()}) > 1:
            lagging.append({"account": account, "nodes": seen})
    return {"accounts": len(accounts), "lagging_accounts": len(lagging),
            "conflicting_accounts": len(conflicting),
            "conflicting_samples": conflicting[:20], "lagging_samples": lagging[:20]}


def state_summary(states):
    """Per-node end state, for the record"""
    return [{"cemented": s["block_count"].get("cemented"), "count": s["block_count"].get("count"),
             "hash": s["final_state"].get("hash"), "pending": s["final_state"].get("pending"),
             "empty": s["final_state"].get("empty")} for s in states]


def stop_run_process_group(process, grace=5):
    """Stop only our own session, including nodes that outlive the client."""
    def alive():
        process.poll()  # Reap the leader; its exit does not imply group exit.
        try:
            os.killpg(process.pid, 0)
            return True
        except ProcessLookupError:
            return False
        except PermissionError:
            # Not signalable by us: treat as gone only if nothing listens
            return bool(occupied_ports())
    # macOS raises EPERM for a group whose remaining members are already
    # reaped or belong to another user; the liveness checks below decide
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        pass
    deadline = time.monotonic() + grace
    while alive() and time.monotonic() < deadline:
        time.sleep(.05)
    if alive():
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
    process.wait(timeout=5)
    deadline = time.monotonic() + 5
    while alive() and time.monotonic() < deadline:
        time.sleep(.05)
    if alive():
        raise RuntimeError('Task process group still exists; preserve run databases')


def occupied_ports():
    occupied = []
    for i in range(6):
        for base in (17075, 17076, 17078):
            port = base + 10 * i
            with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as probe:
                probe.settimeout(.2)
                if probe.connect_ex(("::1", port)) == 0:
                    occupied.append(port)
    return occupied


def workload_args(args):
    command = ["--prs", "6", "--no-prio", "--blocks", str(args.blocks),
               "--accounts", str(args.accounts), "--rate", str(args.rate),
               "--fork-percentage", str(args.forks), "--no-kill"]
    if args.epoch_ms:
        command += ["--epoch-duration-ms", str(args.epoch_ms)]
    return command + args.extra


def baseline_timeout(wall_times, factor, calibration_ceiling):
    if not wall_times:
        return calibration_ceiling, {"rule": "initial baseline calibration ceiling"}
    slowest = max(wall_times)
    return math.ceil(slowest * factor), {
        "rule": "ceil(slowest matching completed baseline wall time * factor)",
        "baseline_count": len(wall_times), "slowest_baseline_wall_secs": slowest,
        "factor": factor,
    }


def load_baseline_times(args):
    times = []
    expected = workload_args(args)
    baseline_sha, client_sha = sha256(args.baseline), sha256(args.client)
    for reference in args.baseline_reference:
        files = [reference] if reference.is_file() else sorted(reference.glob("pair-*-baseline/result.json"))
        accepted = 0
        for path in files:
            result = json.loads(path.read_text())
            command = result.get("command", [])[1:]
            if "--data-dir" in command:
                i = command.index("--data-dir")
                command = command[:i] + command[i + 2:]
            if (result.get("label") == "baseline" and result.get("complete")
                    and result.get("settled_consistent") is True
                    and result.get("node_sha256") == baseline_sha
                    and result.get("client_sha256") == client_sha
                    and command == expected):
                times.append(result["wall_secs"])
                accepted += 1
        if not accepted:
            raise ValueError(f"No completed baseline with matching binaries/workload in {reference}")
    return times


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
    command = [str(args.client), "--data-dir", str(data)] + workload_args(args)
    timeout_seconds, timeout_basis = baseline_timeout(
        args.baseline_wall_times, args.timeout_factor, args.timeout)
    env = os.environ | {"PATH": str(bindir) + os.pathsep + os.environ["PATH"],
                        "RUST_LOG": "nanospam=info", "NANO_LOG": "noansi"}
    result = {"pair": pair, "label": label, "command": command,
              "node": str(binary), "node_sha256": sha256(binary),
              "client_sha256": sha256(args.client), "started": time.time(),
              "timeout_seconds": timeout_seconds, "timeout_basis": timeout_basis,
              "disk_free_before": shutil.disk_usage(args.out).free}
    snapshots = []
    with (directory / "run.log").open("w") as log:
        process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        try:
            try:
                result["exit_code"] = process.wait(timeout=timeout_seconds)
                result["timed_out"] = False
            except subprocess.TimeoutExpired:
                result.update(exit_code=None, timed_out=True)
            # Drain is outside the measured publishing interval. Observe peers
            # before stopping them instead of mistaking PR0's completion for
            # network-wide finality. Fault runs retain their pending evidence.
            if not result["timed_out"] and result["exit_code"] == 0:
                deadline = time.monotonic() + args.settle_seconds
                while True:
                    try:
                        states = [{action: rpc(17076 + 10 * i, {"action": action})
                                   for action in ("block_count", "final_state")}
                                  for i in range(6 - args.absent)]
                        result["ledgers_consistent"] = settled(states, args.forks)
                        result["checkpoints_consistent"] = checkpoints_consistent(
                            states, args.required_checkpoints)
                        result["required_checkpoints"] = args.required_checkpoints
                        result["settled_consistent"] = (result["ledgers_consistent"]
                                                        and result["checkpoints_consistent"])
                        result["end_states"] = state_summary(states)
                    except Exception:
                        result["settled_consistent"] = False
                    if result["settled_consistent"] or time.monotonic() >= deadline:
                        break
                    time.sleep(1)
            # When the end states differ, find out how: lag or conflicting finality
            if args.forks and result.get("settled_consistent") is False:
                frontiers = {}
                for i in range(6 - args.absent):
                    try:
                        reply = rpc(17076 + 10 * i, {"action": "final_state", "frontiers": "true"})
                        frontiers[i] = {a: (h, f) for a, h, f in reply.get("confirmed_frontiers") or []}
                    except Exception as error:
                        frontiers[i] = {}
                        result.setdefault("frontier_errors", []).append(str(error))
                diff = frontier_diff(frontiers)
                (directory / "finality-diff.json").write_text(json.dumps(diff, indent=2) + "\n")
                result["conflicting_accounts"] = diff["conflicting_accounts"]
                result["lagging_accounts"] = diff["lagging_accounts"]
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
                # Failure evidence is captured after the measured settlement
                # verdict. It must not turn a late catch-up into a passing run.
                if result.get("settled_consistent") is False:
                    details = {"captured_at": time.time(), "epochs": []}
                    try:
                        details["checkpoint"] = rpc(17076 + 10 * i, {
                            "action": "final_state", "diagnostic": "true",
                            "checkpoint_only": "true"})
                        for epoch in snapshot.get("final_state", {}).get("epochs", []):
                            details["epochs"].append(rpc(17076 + 10 * i, {
                                "action": "final_state", "epoch": epoch["epoch"]}))
                    except Exception as error:
                        details["collection_error"] = str(error)
                    (directory / f"failure-node-{i}.json").write_text(
                        json.dumps(details, indent=2) + "\n")
                snapshots.append(snapshot)
        finally:
            # Preserve evidence even if OS process cleanup is refused.
            (directory / "rpc.json").write_text(json.dumps(snapshots, indent=2) + "\n")
            (directory / "attempt.json").write_text(json.dumps(result, indent=2) + "\n")
            try:
                stop_run_process_group(process)
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                result["cleanup_error"] = str(error)
            stop_deadline = time.monotonic() + 10
            while occupied_ports() and time.monotonic() < stop_deadline:
                time.sleep(.5)
            if occupied_ports():
                result["cleanup_error"] = "Benchmark ports still occupied after graceful stop"
    result["wall_secs"] = time.time() - result["started"]
    result["disk_free_after"] = shutil.disk_usage(args.out).free
    text = (directory / "run.log").read_text(errors="replace")
    summaries = [line.split("RAI_BENCH_METRICS ", 1)[1]
                 for line in text.splitlines() if "RAI_BENCH_METRICS " in line]
    if summaries:
        metrics = json.loads(summaries[-1])
        result["metrics"] = metrics
        if "nonfork_confirmed" in metrics:
            # The primary measure: blocks published without a fork. Forks
            # are counted apart and do not hold the measurement open.
            result["complete"] = (not result["timed_out"] and result["exit_code"] == 0
                                  and metrics["created"] == args.blocks
                                  and metrics["nonfork_confirmed"] == metrics["nonfork_created"])
            result["goodput"] = metrics["nonfork_confirmed"] / metrics["duration_secs"]
            histogram = metrics["nonfork_histogram_ms"]
            result["measure"] = "non-fork blocks"
            result["fork_unresolved_at_end"] = metrics.get("fork_unresolved_at_end")
        else:
            result["complete"] = (not result["timed_out"] and result["exit_code"] == 0
                                  and metrics["confirmed"] == metrics["created"] == args.blocks)
            result["goodput"] = metrics["confirmed"] / metrics["duration_secs"]
            histogram = metrics["confirmation_histogram_ms"]
            result["measure"] = "all primaries"
        for percentile in (50, 95, 99):
            result[f"p{percentile}_ms"] = quantile(histogram, percentile / 100)
    else:
        result["complete"] = False
    (directory / "rpc.json").write_text(json.dumps(snapshots, indent=2) + "\n")
    (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    if not args.keep_data:
        prune_run_data(directory, result)
    print(json.dumps({k: v for k, v in result.items() if k != "metrics"}), flush=True)
    return result


def prune_run_data(directory, result):
    """User-authorized cleanup after saved evidence and successful process cleanup."""
    directory = Path(directory).resolve()
    data = directory / "data"
    audit = directory / "data-cleanup.json"
    if audit.exists() or not data.exists():
        return
    if result.get("cleanup_error"):
        return
    if data.is_symlink() or data.resolve().parent != directory:
        raise ValueError("Refusing unexpected data directory")
    if not all((directory / name).is_file() for name in ("result.json", "rpc.json", "run.log")):
        raise ValueError("Save result, RPC snapshots and logs before deleting data")
    configs = directory / "saved-config"
    inventory = []
    for source in sorted(data.rglob("*")):
        if source.is_symlink():
            raise ValueError("Refusing data with symlinks")
        if source.is_file():
            relative = source.relative_to(data)
            inventory.append({"path": str(relative), "bytes": source.stat().st_size})
            if source.suffix == ".toml":
                target = configs / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)
    evidence = {"policy": "delete completed run data after evidence; keep data if process cleanup failed",
                "files": inventory, "disk_free_before": shutil.disk_usage(directory).free}
    shutil.rmtree(data)
    evidence["disk_free_after"] = shutil.disk_usage(directory).free
    audit.write_text(json.dumps(evidence, indent=2) + "\n")


def interval(values):
    # Paired percentile bootstrap, with a fixed analysis seed. Exploratory
    # at small n; this is a regression gate, not a universal performance claim.
    rng = random.Random(731)
    samples = sorted(statistics.mean(rng.choices(values, k=len(values))) for _ in range(10000))
    return [samples[250], samples[9749]]


def compare(results, repetitions):
    if not all(r["complete"] for r in results):
        return {"verdict": "FAIL_COMPLETION", "attempts": len(results)}
    if any("cleanup_error" in r for r in results):
        return {"verdict": "CLEANUP_ERROR", "attempts": len(results)}
    if any(r.get("settled_consistent") is False for r in results):
        return {"verdict": "FAIL_SETTLEMENT", "attempts": len(results)}
    if repetitions < 5:
        return {"verdict": "SMOKE_ONLY", "attempts": len(results)}
    pairs = [{r["label"]: r for r in results if r["pair"] == i} for i in range(repetitions)]
    goodput = interval([p["candidate"]["goodput"] / p["baseline"]["goodput"] for p in pairs])
    latency = {}
    for percentile in (50, 95):
        key = f"p{percentile}_ms"
        if any(p["baseline"][key] == 0 for p in pairs):
            return {"verdict": "INCONCLUSIVE_ZERO_LATENCY", "goodput_ratio_ci95": goodput}
        latency[f"p{percentile}_ratio_ci95"] = interval(
            [p["candidate"][key] / p["baseline"][key] for p in pairs])
    verdict = "PASS" if goodput[0] >= .95 and all(ci[1] <= 1.10 for ci in latency.values()) else "INCONCLUSIVE"
    if goodput[1] < .95 or any(ci[0] > 1.10 for ci in latency.values()):
        verdict = "REGRESSION"
    diagnostics = {}
    if all(p["baseline"]["p99_ms"] > 0 for p in pairs):
        diagnostics["p99_ratio_ci95"] = interval(
            [p["candidate"]["p99_ms"] / p["baseline"]["p99_ms"] for p in pairs])
    return {"verdict": verdict, "gate_version": "p50-p95-v1",
            "goodput_ratio_ci95": goodput, **latency, "diagnostic_only": diagnostics}



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
    parser.add_argument("--timeout", type=int, default=240,
                        help="Ceiling only for initial baseline calibration when no reference exists")
    parser.add_argument("--timeout-factor", type=float, default=1.5)
    parser.add_argument("--baseline-reference", type=Path, action="append", default=[],
                        help="Prior run directory or baseline result.json matching binaries/workload")
    parser.add_argument("--settle-seconds", type=int, default=30)
    parser.add_argument("--required-checkpoints", type=int, default=0,
                        help="Require this many identical installed checkpoints before settlement")
    parser.add_argument("--absent", type=int, default=0)
    parser.add_argument("--min-free-gib", type=float, default=8)
    parser.add_argument("--keep-data", action="store_true",
                        help="Keep generated node data after the run")
    parser.add_argument("--stop-on-failure", action="store_true",
                        help="Stop the development gate at the first failed attempt; retain its evidence")
    parser.add_argument("--candidate-only", action="store_true",
                        help="Run only candidate attempts under --timeout; no comparison or pass is produced")
    parser.add_argument("extra", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.extra[:1] == ["--"]:
        args.extra.pop(0)
    for name in ("baseline", "candidate", "client", "out"):
        setattr(args, name, getattr(args, name).resolve())
    if args.required_checkpoints < 0:
        parser.error("--required-checkpoints must be nonnegative")
    if args.timeout_factor <= 1:
        parser.error("--timeout-factor must exceed 1")
    args.baseline_wall_times = load_baseline_times(args)
    args.out.mkdir(parents=True, exist_ok=False)
    manifest = {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()}
    manifest.update(gate_version="p50-p95-v1", primary_latency_percentiles=[50, 95],
                    latency_margin=1.10, goodput_margin=.95,
                    platform=platform.platform(), machine=platform.machine(),
                    harness_sha256=sha256(Path(__file__)),
                    workload_seed=None,
                    workload_note="Shared pinned client and parameters; HEAD generator is not seeded.")
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2, default=str) + "\n")
    results = []
    for pair in range(args.repetitions):
        order = [("baseline", args.baseline), ("candidate", args.candidate)]
        if pair % 2:
            order.reverse()
        if args.candidate_only:
            order = [("candidate", args.candidate)]
        for label, binary in order:
            ports = occupied_ports()
            if ports:
                verdict = {"verdict": "PORTS_OCCUPIED", "ports": ports,
                           "completed_attempts": len(results)}
                (args.out / "comparison.json").write_text(json.dumps(verdict, indent=2) + "\n")
                print(json.dumps(verdict), flush=True)
                return 2
            free = shutil.disk_usage(args.out).free
            if free < args.min_free_gib * 1024 ** 3:
                verdict = {"verdict": "DISK_SPACE_LIMIT", "free_bytes": free,
                           "completed_attempts": len(results)}
                (args.out / "comparison.json").write_text(json.dumps(verdict, indent=2) + "\n")
                print(json.dumps(verdict), flush=True)
                return 2
            results.append(run(args, label, binary, pair))
            last = results[-1]
            if label == "baseline" and last["complete"] and last.get("settled_consistent") is True:
                args.baseline_wall_times.append(last["wall_secs"])
            if not args.baseline_wall_times and label == "baseline":
                verdict = {"verdict": "BASELINE_CALIBRATION_FAILED", "attempts": len(results)}
                (args.out / "comparison.json").write_text(json.dumps(verdict, indent=2) + "\n")
                print(json.dumps(verdict), flush=True)
                return 1
            if args.stop_on_failure and (not last["complete"] or "cleanup_error" in last
                                        or last.get("settled_consistent") is False):
                verdict = compare(results, args.repetitions)
                verdict["stopped_early"] = True
                (args.out / "comparison.json").write_text(json.dumps(verdict, indent=2) + "\n")
                print(json.dumps(verdict), flush=True)
                return 1
    if args.candidate_only:
        verdict = {"verdict": "CANDIDATE_ONLY", "attempts": len(results),
                   "complete": [r["complete"] for r in results], "performance_claim": False}
    else:
        verdict = compare(results, args.repetitions)
    (args.out / "comparison.json").write_text(json.dumps(verdict, indent=2) + "\n")
    print(json.dumps(verdict), flush=True)
    return 0 if verdict["verdict"] in ("PASS", "SMOKE_ONLY") else 1


if __name__ == "__main__":
    raise SystemExit(main())
