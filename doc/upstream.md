# Upstream Merge Status

## Current Merge Status

These are the most recent commits that have been reviewed and merged into RsNano or put into the unmerged table below:

- [x] 54a8196e4dbe8c328500ac3d7dd130764544be5c
- [x] 765fbef0ea133b048eb62305458fd5871f9e7451
- [x] 35a0b6f88c3efb2541c4fe2a654d8819a6519ec0
- [x] 2597405dff33801ca522b244b043a2654eb677eb
- [x] fa5fed3bca99934a2de92cf82a3e158d3fb7ea4b
- [x] a04a2496e36d2c90f004ebbdeb39ce7d6b1e4bc9
- [x] 6f405e005fc79c4d25af031c2b2854e25d95d060
- [x] f43442ddd361cc4c31288d64d31b4c63b7493769

## Unmerged Upstream Changes

|Title                                           |Commit                                  |Pull Request                                         |Notes|
|------------------------------------------------|----------------------------------------|-----------------------------------------------------|-----|
|Handshake Timeout                               |a7610bd0844aaf4cd8cd8b110119d6b242a67bc3|https://github.com/nanocurrency/nano-node/pull/4919/ |Discard the connection if the handshake doesn't complete within 2s|
|Activate largest-gap optimistic elections first |aa4ca10ba22fa79b6109932ecbea74d246753319|https://github.com/nanocurrency/nano-node/pull/4939/ |There is a follow up bugfix commit 46b4aa14d8a526491273cbdd2a9d6d119a9b4cdf|
|AEC Rework                                      |18506b3d1ba4250216d4868157e31d4fd8271751|https://github.com/nanocurrency/nano-node/pull/4943/ ||
|AEC Races (needs AEC Rework first!)             |285ae0ef10421383ac69504a61d9e981f4e7d734|https://github.com/nanocurrency/nano-node/pull/4945/ ||
|CLI Benchmarks                                  |53a2f4e3485a9f0f26fcebb2fd80a7c3bbfe824d|https://github.com/nanocurrency/nano-node/pull/4953/ ||
|Shared Priority Pool                            |964ed6e759fabfb5edb872aeea7896f06c5e5542|https://github.com/nanocurrency/nano-node/pull/4954/ ||
|Use Max Filedescriptor Limit                    |8de5920578fc82129467f21e286dddbcf8a16de3|https://github.com/nanocurrency/nano-node/pull/4968/ ||
|Don't sample weights if below threshold         |997b26279e4d5e20ef510e3ab507594977ed46a0|https://github.com/nanocurrency/nano-node/pull/4969/ ||
|TXN tracking                                    |34c76330c520035ce296f435d193da2a96713456|https://github.com/nanocurrency/nano-node/pull/4982/ ||
|Super Rebroadcaster                             |6d53b5f89f5feec87c111153ee5e86b36d4f9502|https://github.com/nanocurrency/nano-node/pull/4985/ ||
|Log when peered stake below quorum              |3cfae9c17e318f3eefbc8530a28892d4aaafc77d|https://github.com/nanocurrency/nano-node/pull/4991/ ||
|Rate-limit low online weight warning            |cc4008185c99fccf59e007c0a79ec799be18751d|https://github.com/nanocurrency/nano-node/pull/4999/ ||
|Priority Scheduler Stress Test                  |3681e28a60f0556e6df002ce80cbf54400b1b0b6|https://github.com/nanocurrency/nano-node/pull/5007/ ||
|Disable RequestAggregator if not voting         |0274cf73f56989c294b2c2fa83a67a000530b508|https://github.com/nanocurrency/nano-node/pull/5022/ ||
|Bounded DFS                                     |6656b457aee5c2956ecc1fa76462a59023cf2649|https://github.com/nanocurrency/nano-node/pull/5030/ ||
|Extend Telemetry Data                           |2597405dff33801ca522b244b043a2654eb677eb|https://github.com/nanocurrency/nano-node/pull/5035/ ||
|Add work_server_use_peers field to config       |fa5fed3bca99934a2de92cf82a3e158d3fb7ea4b|https://github.com/nanocurrency/nano-node/pull/5017/ ||
|Exchange Node Capabilities                      |6f405e005fc79c4d25af031c2b2854e25d95d060|https://github.com/nanocurrency/nano-node/pull/5043/ ||
|Extract `ledger::block_find`                    |f43442ddd361cc4c31288d64d31b4c63b7493769|https://github.com/nanocurrency/nano-node/pull/5045/ ||
