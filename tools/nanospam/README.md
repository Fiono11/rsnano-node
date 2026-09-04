# Tool: nanospam

## Example Usage
Run a single node spam test, starting at 1 bps and continually increase bps. (rsnano_node has to be in $PATH)
```
nanospam
```

Run a single node, publish 1000 blocks at 200 bps and wait until confirmed
```
nanospam --blocks 1000 --rate 200
```

Run a network spam test with 4 principal reps, starting at 1 bps.
```
nanospam --prs 4
```

Run a 4 PR network and start with 1000 bps, then increase by 100 bps every 5 seconds:
```
nanospam --prs 4 --rate 1000+100@5s
```

Publish 1000 blocks to a 4 PR network without ensuring that the previous block got confirmed
```
nanospam --prs 4 --rate 1000 --unconfirmed
```

Test with change blocks, instead of sends/receives
```
# Create ledgers with open accounts:
nanospam --prs 4 --rate 3000 --blocks 50000

# Start change block spam
nanospam --prs 4 --rate 3000 --blocks 50000 --sync --change --unconfirmed
```

Only create the node folders in ~/NanoSpam and configure them and create the node wallets, then stop.
```
nanospam --prs 4 --setup-only
```

Reuse the previous node folders and database. Continue spam with the existing databases
```
nanospam --prs 4 --rate 1000 --sync
```

Don't start any nodes, but attach to nodes that have to be running already
```
nanospam --prs 4 --rate 1000 --attach --sync
```

When built with `rai_protocol`, nanospam normally passes the ordered PR public
keys to the nodes in `NANO_RAI_FIXED_COMMITTEE`. The nodes divide `Amount::MAX`
equally between those keys (with any indivisible raw remainder assigned to PR0)
and do not change those voting weights when blocks are inserted or rolled back.
Fork runs (`--fork-percentage > 0`) instead use the legacy startup sequence, in
which genesis initially owns quorum weight while the PR wallets are funded one
at a time. When using `--attach`, nanospam does not control the node environment.
After each generated fork, nanospam generates one non-forked successor from the
terminated winner before applying the configured fork probability again.

## Overview
Nanospam creates random blocks (without PoW) and sends them with increasing BPS to a local test node.

This tool is still early work and rough to work with, but it can already be used for measuring node performance.

This is how it works:
- It connects to a local running nano node in the test network via the default port and the websocket port.
- This nano node has the genesis key in its wallet and PoW disabled
- It crates 500k random account keys
- It sends 100m nano from genesis to one of the test accounts
- Then it publishes random blocks (random sends and receives between the test accounts). A new block is only created if the previous got confirmed.
- It waits for all blocks to be confirmed (via websocket feedback)
- If a block isn't confirmed within 10s it gets republished
- It starts with 50 bps and continually increases with a rate of 1000/min


Because PoW is disabled, creating blocks is very fast. On my old and slow laptop it can consistently create 51k bps on a single core.

## Running it

### Single Node Spam Test
```

Usage: nanospam [OPTIONS]

Options:
      --prs <PRS>        Number of principal representatives [default: 1]
      --setup-only       Only create the node config files and set up the wallets, then exit
      --attach           Attach to an already running node
      --rate <RATE>      Block rate in the form "1000+50@3s" or "1000"
      --blocks <BLOCKS>  Number of blocks to publish
      --unconfirmed      Don't wait for a block to get confirmed before publishing the next block
      --sync             Query frontiers of the spam accounts before starting spam
      --change           Only publish change blocks. This requires --sync
  -h, --help             Print help
```

Running it with no arguments will start a single node that is a PR. The node data is in `~/NanoSpam/pr0/`. 
`nanospam` does all the configuration for you! It creates the node and rpc configuration files and it
inserts the genesis key into the wallet


### Running it with custom nodes
You can start the nodes yourself. This enables spam tests with docker images for example. You need to set the following environment variables:
```bash
export NANO_TEST_GENESIS_BLOCK='{
    "type": "open",
    "account": "nano_3nroioygg54nusrmyun4woimqex36sp3drnctdt5955uqu47fxbkrxk7n7ne",
    "source": "D315857CE70C54DE713F6E82E5613BB3A1266C15E28AD2F4338C7BBEC456F532",
    "representative": "nano_3nroioygg54nusrmyun4woimqex36sp3drnctdt5955uqu47fxbkrxk7n7ne",
    "signature": "3F6792C2DC623DF2E8643777160AB983B66B337E2478E13D2C3448126A8F4CD8DCCD19803C158A057FA44060AE0EFC09B1C311CB4FBF42F8D240610B38F56E08",
    "work": "70FEF01F7EC45DEC"
}'

export NANO_TEST_GENESIS_PRV="49643F9B10CA1AA34F9AF8ED4AABD29F436104CCC375974B108534A48EAE3FE1"
export NANO_TEST_EPOCH_1="0"
export NANO_TEST_EPOCH_2="0"
export NANO_TEST_EPOCH_2_RECV="0"
```

If you test a network of nano_node, you have to make sure that the nodes are properly connected. 
If you run the nodes after for example "nanospam --prs 4 --setup-only" you have to issue a keepalive RPC to the 
nodes, so that they connect.

## Misc
If you would like to know how fast your computer can generate blocks on a single core, then run:
```
cd tools/nanospam/
cargo test --release -- --nocapture --ignored benchmark
```
It will output something like this:
```running 1 test
Created 50000 blocks. 53958 bps
Created 50000 blocks. 53827 bps
Created 50000 blocks. 53793 bps
Created 50000 blocks. 53779 bps
Created 50000 blocks. 53717 bps
```
