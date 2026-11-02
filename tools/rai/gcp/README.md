# RAI across machines on Google Cloud

Real link latency instead of `--network-delay-ms`. Each representative runs on its
own VM; a controller runs `nanospam` and `run_gate_b.py` and starts the nodes over
ssh. The free trial caps running VMs at 8 vCPUs, so the fleet is:

| VM | Type | Role |
|----|------|------|
| `rai-ctl` | n1-standard-2 (built as n1-standard-8) | nanospam, harness, the build |
| `rai-pr0` | n1-standard-1, same zone as the controller | PR0 (genesis, the websocket nanospam measures through) |
| `rai-pr1`..`rai-pr5` | n1-standard-1, one zone each | PR1..PR5 |

Six nodes, not seven: f = 1, p = 1 gives N = 3f + 2p + 1 = 6 seats, the same
committee as the local matrix. In the `byz1` and `offline1` variants only the
first five nodes run; `rai-pr5` idles.

Layouts (`fleet.sh nodes LAYOUT`):

- `continent`: us-central1, us-east1, us-west1, us-east4, us-west2, northamerica-northeast1 (RTT ~20-70 ms)
- `global`: us-central1, us-east1, europe-west1, europe-west4, asia-east1, australia-southeast1 (RTT ~100-250 ms)

The default VPC routes internal addresses across regions and its
`default-allow-internal` firewall rule opens every port between the VMs; the
nodes need no external address (the harness copies the binaries to them).

## Setup

From your machine, with `gcloud` set to the trial project:

```bash
tools/rai/gcp/fleet.sh controller                 # 8 vCPUs for the build
gcloud compute ssh rai-ctl --zone us-central1-a
```

On the controller:

```bash
sudo apt update && sudo apt install -y build-essential pkg-config git python3 tmux
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . ~/.cargo/env
git clone -b rai_multihost https://github.com/Fiono11/rsnano-node && cd rsnano-node
cargo build --release -p rsnano_cli -p nanospam --features rsnano_cli/rai_protocol
strings target/release/rsnano | grep -c EPOCH_START   # > 0, else it is not a RAI build
```

Back on your machine (the controller must shrink before the nodes fit the cap):

```bash
tools/rai/gcp/fleet.sh shrink
tools/rai/gcp/fleet.sh nodes continent
tools/rai/gcp/fleet.sh hosts > hosts-continent.txt
gcloud compute scp hosts-continent.txt rai-ctl:rsnano-node/ --zone us-central1-a
```

On the controller, check that ssh to every node works without a prompt (the
first contact records the host key):

```bash
for ip in $(grep -v '^#' hosts-continent.txt); do ssh -o StrictHostKeyChecking=accept-new $ip true && echo "$ip ok"; done
```

## Runs

Inside `tmux`, from `~/rsnano-node`:

```bash
# Find the saturation knee first: 1 vCPU per node saturates far below the M1
for rate in 250 500 750; do
  python3 tools/rai/run_gate_b.py doc/benchmarks/gcp-continent-knee/nofork-$rate \
    --hosts hosts-continent.txt --rate $rate --blocks 15000 --accounts 15000
done

# Then the matrix below the knee (arms take run_gate_b flags after ':')
python3 tools/rai/run_eval_matrix.py doc/benchmarks/gcp-continent-matrix --reps 3 \
  --arm rai=target/release:--hosts=hosts-continent.txt:--rate=400 \
  --arm pause=target/release:--hosts=hosts-continent.txt:--rate=400:--epoch-transition=pause
```

Every `result.json` records `hosts` and `rtt_ms` (the ping matrix between the
node machines, measured before the run), so each figure can state the latency
it ran under. Keep `--network-delay-ms` at 0.

Copy the results home and stop paying:

```bash
gcloud compute scp --recurse rai-ctl:rsnano-node/doc/benchmarks/gcp-continent-matrix doc/benchmarks/ --zone us-central1-a
tools/rai/gcp/fleet.sh delete
```

## What runs where

- nanospam writes every node's config locally, then copies `pr<i>/` to the same
  path on the node's machine with a `run-node.sh` that records the node's pid and
  `exec`s `~/rai-bin/<build>/rsnano`. The ssh client stays attached as the node's
  stderr, so the `EPOCH_*` diagnostics still land in `run.log`.
- `--restart` kills a remote node by that pid (SIGKILL) and starts it again over ssh.
- The harness polls each node's RPC at its address, fetches `diagnostics.log` and
  `signed-votes.log` before the vote audit, then kills the remote nodes and deletes
  their data directories (`--keep` leaves them).
- A hosts line `ADDRESS local` runs that node on the controller instead.
- Latency is still measured by nanospam on the controller: publish to every node,
  confirmation as PR0's websocket reports it. PR0 sits in the controller's zone so
  that hop stays sub-millisecond; the other five are remote.
