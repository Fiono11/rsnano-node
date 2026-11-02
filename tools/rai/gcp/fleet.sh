#!/usr/bin/env bash
# RAI across machines on Google Compute Engine, within the free trial's
# 8-vCPU cap: a controller (nanospam + run_gate_b.py, n1-standard-2) and six
# node VMs (n1-standard-1, no external address) in the zones of a layout.
# Run from a machine with gcloud set to the project. See README.md.
#
#   fleet.sh controller          create the controller as n1-standard-8 to build on
#   fleet.sh shrink              resize the controller to n1-standard-2 (after the build)
#   fleet.sh nodes LAYOUT        create rai-pr0..rai-pr5 (LAYOUT: continent | global)
#   fleet.sh hosts               print the --hosts file (PR order)
#   fleet.sh delete              delete every rai-* VM
set -euo pipefail

CONTROLLER=rai-ctl
CONTROLLER_ZONE=${CONTROLLER_ZONE:-us-central1-a}
IMAGE=(--image-family=ubuntu-2404-lts-amd64 --image-project=ubuntu-os-cloud)

# PR0 shares the controller's zone: nanospam measures confirmations through
# PR0's websocket, so that hop is not part of the latency
layout_zones() {
    case "$1" in
        continent) echo "$CONTROLLER_ZONE us-east1-b us-west1-a us-east4-a us-west2-a northamerica-northeast1-a" ;;
        global) echo "$CONTROLLER_ZONE us-east1-b europe-west1-b europe-west4-a asia-east1-a australia-southeast1-a" ;;
        *) echo "unknown layout $1 (continent | global)" >&2; exit 2 ;;
    esac
}

case "${1:-}" in
    controller)
        gcloud compute instances create "$CONTROLLER" --zone="$CONTROLLER_ZONE" \
            --machine-type=n1-standard-8 "${IMAGE[@]}" \
            --boot-disk-size=100GB --boot-disk-type=pd-balanced
        ;;
    shrink)
        gcloud compute instances stop "$CONTROLLER" --zone="$CONTROLLER_ZONE"
        gcloud compute instances set-machine-type "$CONTROLLER" --zone="$CONTROLLER_ZONE" --machine-type=n1-standard-2
        gcloud compute instances start "$CONTROLLER" --zone="$CONTROLLER_ZONE"
        ;;
    nodes)
        zones=($(layout_zones "${2:?layout: continent | global}"))
        # The controller's own key opens the nodes: nanospam starts them over ssh
        key=$(gcloud compute ssh "$CONTROLLER" --zone="$CONTROLLER_ZONE" --command \
            'test -f ~/.ssh/id_ed25519 || ssh-keygen -q -t ed25519 -N "" -f ~/.ssh/id_ed25519; echo "$(whoami):$(cat ~/.ssh/id_ed25519.pub)"')
        for i in "${!zones[@]}"; do
            gcloud compute instances create "rai-pr$i" --zone="${zones[$i]}" \
                --machine-type=n1-standard-1 "${IMAGE[@]}" \
                --boot-disk-size=20GB --boot-disk-type=pd-balanced \
                --no-address --metadata=ssh-keys="$key" &
        done
        wait
        ;;
    hosts)
        echo "# rai-pr0..rai-pr5 internal addresses, PR order"
        for i in 0 1 2 3 4 5; do
            gcloud compute instances list --filter="name=rai-pr$i" --format='value(networkInterfaces[0].networkIP)'
        done
        ;;
    delete)
        gcloud compute instances list --filter='name~^rai-' --format='value(name,zone)' |
            while read -r name zone; do
                gcloud compute instances delete "$name" --zone="$zone" --quiet &
            done
        wait
        ;;
    *)
        sed -n '2,13p' "$0"
        exit 2
        ;;
esac
