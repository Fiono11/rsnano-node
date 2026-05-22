<p style="text-align:center;"><img src="/doc/images/logo.svg" width"300px" height="auto" alt="Logo"></p>


[![Unit Tests](https://github.com/simpago/rsnano-node/actions/workflows/unit_tests.yml/badge.svg)](https://github.com/simpago/rsnano-node/actions/workflows/unit_tests.yml)
[![codecov](https://codecov.io/gh/rsnano-node/rsnano-node/graph/badge.svg?token=LIATNV5NBP)](https://codecov.io/gh/rsnano-node/rsnano-node)
[![Discord](https://img.shields.io/badge/discord-join%20chat-orange.svg)](https://discord.gg/kBwvAyxEWE)


### What is RsNano?

RsNano is a full Nano/Banano node written in Rust.

### What is Nano?

Nano is a digital payment protocol designed to be accessible and lightweight, 
with a focus on removing inefficiencies present in other cryptocurrencies. 
With ultrafast transactions and zero fees on a secure, green and decentralized 
network, this makes Nano ideal for everyday transactions.

### Links & Resources

* [RsNano Website](https://rsnano.com)
* [Discord Chat](https://discord.gg/kBwvAyxEWE)
* [Twitter](https://twitter.com/gschauwecker)

### Installation

## Option 1: Run the official docker image

    docker run -p 7075:7075 -v ~/Nano:/root/Nano simpago/rsnano:V2.0 --network=live node run

## Option 2: Build your own docker image

    docker build -f tools/scripts/docker/node/Dockerfile -t rsnano-node https://github.com/simpago/rsnano-node.git#releases/v2

    docker run -p 7075:7075 -v ~/Nano:/root/Nano rsnano-node:latest --network=live node run

## Option 3: Build from source

Currently you can only build RsNano on Linux and on Mac.

To just build and run the rsnano_node:

    git clone https://github.com/simpago/rsnano-node.git
    cd rsnano-node
    cargo run --release --bin rsnano -- --network=live node run

To install and run the rsnano_node executable:

    git clone https://github.com/simpago/rsnano-node.git
    cd rsnano-node
    cargo install --path cli
    rsnano --network=live node run

## Running it with a GUI

You can even run an RsNano node with a GUI that looks like this:
![RsNano Insight App](https://raw.githubusercontent.com/rsnano-node/rsnano-node/refs/heads/develop/doc/insight_app.png)

Run this command:

    cargo run --release --bin rsnano-insight

### Running a Banano node

RsNano can be compiled to be a Banano node too! Use the feature "banano" like this:

    cargo run --release --features banano --bin rsban -- node run

Or run it with the graphical interface:

    cargo run --release --features banano --bin rsban-insight


### Contact us

We want to hear about any trouble, success, delight, or pain you experience when
using RsNano. Let us know by [filing an issue](https://github.com/simpago/rsnano-node/issues), or joining us on [Discord](https://discord.gg/kBwvAyxEWE).

# The codebase

Have a look at the [AI generated documentation of the codebase](https://deepwiki.com/rsnano-node/rsnano-node).

The Rust code is structured according to A-frame architecture and is built with nullable infrastructure. 
This design and testing approach is [extensively documented on James Shore's website](http://www.jamesshore.com/v2/projects/nullables/testing-without-mocks)

Watch James Shore's presentation of nullables on YouTube: [Testing Without Mocks - James Shore | Craft Conference 2024](https://www.youtube.com/watch?v=GjZg6lDBKkk)

The following diagram shows how the crates are organized. The crates will be split up more when the codebase grows.

```mermaid
flowchart TD
    main --> daemon
    daemon --> node
    daemon --> rpc_server
    daemon --> websocket_server
    rpc_server --> node
    rpc_server --> rpc_messages
    rpc_client --> rpc_messages
    rpc_messages --> types
    node --> ledger
    node --> network_protocol
    node --> wallet
    network_protocol --> messages
    network_protocol --> network
    websocket_server --> websocket_messages
    websocket_server --> node
    websocket_messages --> types
    websocket_client --> websocket_messages
    messages --> work
    messages --> utils
    network --> utils
    ledger --> store_lmdb
    ledger --> work
    ledger --> utils
    store_lmdb --> types
    work --> types
    work --> utils
    wallet --> ledger

    subgraph rpc
        rpc_messages
        rpc_server
        rpc_client
    end

    subgraph websocket
        websocket_messages
        websocket_server
        websocket_client
    end

    subgraph nullables
        fs
        clock
        random
        tcp
        lmdb
        http_client
        console
        env
        output_tracker
    end
```

* `main`: The node executable.
* `daemon`: Starts the node and optionally the RPC server.
* `node`:The node implementation.
* `rpc_server`: Implemenation of the RPC server.
* `websocket_server`: Implemenation of the websocket server.
* `wallet`: Wallet implementation. It manages multiple wallets which can each have multiple accounts.
* `ledger`: Ledger implementation. It is responsible for the consinstency of the data stores.
* `store_lmdb`: LMDB implementation of the data stores.
* `messages`: Message types that nodes use for communication.
* `network`: Manage outbound/inbound TCP channels to/from other nodes.
* `work`: Proof of work generation via CPU or GPU
* `types`: Contains the basic types like `BlockHash`, `Account`, `KeyPair`,...
* `utils`: Contains utilities like stats
* `nullables`: Nullable wrappers for infrastructure libraries.

