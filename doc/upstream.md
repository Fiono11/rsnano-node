# Upstream Merge Status

## Current Merge Status

These are the most recent commits that have been reviewed and merged into RsNano or put into the unmerged table below:

- [x] 8f73988fa8d5554bcfa8a91581cb3be04c2091fb
- [x] 651f2ea74631f5ca80a326797b4b66f56807740b
- [x] 7ba55f4387f3eb483c8a63859bbb613fe2cc112e
- [x] dbdbf7ac489ab5b2e4bc5794489545db47a1a47a
- [x] 381999f26609e9d34fabe904325f05c3a0d11a2b

## Unmerged Upstream Changes

|Priority|Title                                            |Commit                                  |Pull Request                                         |Notes|
|--------|-------------------------------------------------|----------------------------------------|-----------------------------------------------------|-----|
|:zap:   |AEC Rework                                       |18506b3d1ba4250216d4868157e31d4fd8271751|https://github.com/nanocurrency/nano-node/pull/4943/ ||
|:zap:   |AEC Races (needs AEC Rework first!)              |285ae0ef10421383ac69504a61d9e981f4e7d734|https://github.com/nanocurrency/nano-node/pull/4945/ ||
|:zzz:   |CLI Benchmarks                                   |53a2f4e3485a9f0f26fcebb2fd80a7c3bbfe824d|https://github.com/nanocurrency/nano-node/pull/4953/ ||
|:zap:   |Shared Priority Pool                             |964ed6e759fabfb5edb872aeea7896f06c5e5542|https://github.com/nanocurrency/nano-node/pull/4954/ ||
|:zzz:   |TXN tracking                                     |34c76330c520035ce296f435d193da2a96713456|https://github.com/nanocurrency/nano-node/pull/4982/ ||
|:zzz:   |Super Rebroadcaster                              |6d53b5f89f5feec87c111153ee5e86b36d4f9502|https://github.com/nanocurrency/nano-node/pull/4985/ ||
|:zzz:   |Priority Scheduler Stress Test                   |3681e28a60f0556e6df002ce80cbf54400b1b0b6|https://github.com/nanocurrency/nano-node/pull/5007/ ||
|:fire:  |Disable RequestAggregator if not voting          |0274cf73f56989c294b2c2fa83a67a000530b508|https://github.com/nanocurrency/nano-node/pull/5022/ ||
|:fire:  |Bounded DFS                                      |6656b457aee5c2956ecc1fa76462a59023cf2649|https://github.com/nanocurrency/nano-node/pull/5030/ ||
|:fire:  |Extend Telemetry Data                            |2597405dff33801ca522b244b043a2654eb677eb|https://github.com/nanocurrency/nano-node/pull/5035/ ||
|:zap:   |Add work_server_use_peers field to config        |fa5fed3bca99934a2de92cf82a3e158d3fb7ea4b|https://github.com/nanocurrency/nano-node/pull/5017/ ||
|:fire:  |Exchange Node Capabilities                       |6f405e005fc79c4d25af031c2b2854e25d95d060|https://github.com/nanocurrency/nano-node/pull/5043/ ||
|:zap:   |Extract `ledger::block_find`                     |f43442ddd361cc4c31288d64d31b4c63b7493769|https://github.com/nanocurrency/nano-node/pull/5045/ ||
|:zap:   |Extract vote eligibility logic into voting_policy|cc768f39965084d99e043f9c779a44e0ec440b15|https://github.com/nanocurrency/nano-node/pull/5044/ ||
|:zap:   |Replace request aggregator with vote replier     |ff4f744ce6ebe3b3e47a2cee13b272051fb32623|https://github.com/nanocurrency/nano-node/pull/5050/ ||
|:zap:   |Vote generator rework                            |ccad7d2d72ea30024f1bbc6003fc25ebb53d9263|https://github.com/nanocurrency/nano-node/pull/5054/ ||
|:zap:   |Add crawler refresh and block_view crawl         |1fd1df44bb7a0fead03914b9d2eb424e2fe838e1|https://github.com/nanocurrency/nano-node/pull/5042/ ||
|:zap:   |Add topo_height field to block sideband          |25d256919f78b9b566ec387852ec279d146c7c83|https://github.com/nanocurrency/nano-node/pull/5057/ ||
|:zap:   |Cache rep keys for foreach_representative        |2a011b4859fcee1b46aeb7175915f6b163ed2fff|https://github.com/nanocurrency/nano-node/pull/5060/ ||
|:zzz:   |Add --enable_rpc startup flag                    |d4a9ab91a9dd2ecfb6c52b46ef0cc7d1fdf07a24|https://github.com/nanocurrency/nano-node/pull/5065/ ||
|:zap:   |Extract wallet storage backend                   |5b6c408c95534636b8841eef597428955739139b|https://github.com/nanocurrency/nano-node/pull/5064/ ||
|:zap:   |Encapsulate wallet key usage behind wallet_cipher|34919df0499068a320e072c8da211abe0bcf2807|https://github.com/nanocurrency/nano-node/pull/5067/ ||
|:zap:   |Add --database_upgrade CLI option                |39fc4db9cc7829b769b0ecb33b20e3ebe3801ee7|https://github.com/nanocurrency/nano-node/pull/5068/ ||
|:zap:   |Ledger topology index for topological bootstrap  |d9170bbb85c1b3879f35320fc0ae083fb60dbd47|https://github.com/nanocurrency/nano-node/pull/5069/ ||
|:zap:   |Expose peer capabilities in peers RPC            |1ae7d9874973ab6e50af67034887f5040e332679|https://github.com/nanocurrency/nano-node/pull/5077/ ||
|:zap:   |Batch bootstrap pull queries                     |28640d1880357009564c53f9da119e7ad797cb8a|https://github.com/nanocurrency/nano-node/pull/5080/ ||
|:zzz:   |Add flag to disable elections                    |2722324dea5c6c454c4181b04058a9f6bba0f059|https://github.com/nanocurrency/nano-node/pull/5081/ ||
|:zzz:   |Add flag to disable bounded backlog              |2e3f92f8094e89b02bcfd0d16ad541fada6221f2|https://github.com/nanocurrency/nano-node/pull/5084/ ||
|:zap:   |Scan backlog conf heights in lockstep            |0b848dbf2f1523ab369b9dc8cd654ab90f0e494a|https://github.com/nanocurrency/nano-node/pull/5085/ ||
|:fire:  |Restore oversized read buffer                    |a945ab419dbb7c504b2a69021116290186f7a8b9|https://github.com/nanocurrency/nano-node/pull/5090/ ||
|:zap:   |fix-watch-only-representative-abort              |96360d13208a888cf30305d4e3a1385cf4ac3ad5|https://github.com/nanocurrency/nano-node/pull/5098/ ||
|:zap:   |Stabilise flaky multiple_representatives test    |cf0611e0af4ba4b56707955aa4f6e7fd78aa4042|https://github.com/nanocurrency/nano-node/pull/5097/ ||
|:zap:   |Fix race condition in confirm_quorum test        |d91fa07c42e47264f5b0fcc815db837858df0bb5|https://github.com/nanocurrency/nano-node/pull/5096/ ||
|:zap:   |Stabilise flaky fork_replacement_tally           |6acad638498d3f152ef6164f72bee7182aa32e35|https://github.com/nanocurrency/nano-node/pull/5091/ ||
|:zap:   |Report preconfigured peer connect failures       |8f73988fa8d5554bcfa8a91581cb3be04c2091fb|https://github.com/nanocurrency/nano-node/pull/5092/ ||
|:zap:   |Extract bootstrap verify policy                  |651f2ea74631f5ca80a326797b4b66f56807740b|https://github.com/nanocurrency/nano-node/pull/5095/ ||
|:zzz:   |Add loopback-only peer mode                      |7ba55f4387f3eb483c8a63859bbb613fe2cc112e|https://github.com/nanocurrency/nano-node/pull/5093/ ||
|:zzz:   |Add `--database_info` CLI command                |dbdbf7ac489ab5b2e4bc5794489545db47a1a47a|https://github.com/nanocurrency/nano-node/pull/5076/ ||

Priority: High: :fire:, Medium :zap:, Low :zzz:
