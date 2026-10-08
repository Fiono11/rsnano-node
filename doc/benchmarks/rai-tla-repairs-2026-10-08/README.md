# RAI TLA+ repairs: gate B A/B (2026-10-08)

Base 4cae51b0b vs new (repairs). Default gate B: 6 equal-weight PRs, 45k blocks at 2000/s, 8 s epochs. One run per line; ab = before the manifest chunk fix 4823ba8bb, fix = after it (new binary only).

## ab
```
nofork-base settled correct=True cps=1952.3 p50/95/99=104/197/339 fork_conf=0/0 close_med/max=1115/3213 rounds=[[0, '0'], [1, '0'], [2, '0']] decided=[0, 1, 2] uncemented=[0]
nofork-new settled correct=True cps=1939.0 p50/95/99=105/326/699 fork_conf=0/0 close_med/max=1440/2840 rounds=[[0, '0'], [1, '0'], [2, '0']] decided=[0, 1, 2] uncemented=[0]
fork5-base settled correct=True cps=1819.8 p50/95/99=114/399/660 fork_conf=71/2228 close_med/max=2043/5338 rounds=[[0, '0'], [1, '0'], [2, '0']] decided=[0, 1, 2] uncemented=[2151]
fork5-new failed correct=False cps=1774.6 p50/95/99=160/590/947 fork_conf=1457/2168 close_med/max=4540/5479 rounds=[[0, '0'], [1, '0'], [2, '1']] decided=[0, 1, 2] uncemented=[99, 100]
fork5byz1-base settled correct=True cps=1821.4 p50/95/99=122/300/415 fork_conf=64/2336 close_med/max=3930/5714 rounds=[[0, '0'], [1, '0'], [2, '1']] decided=[0, 1, 2] uncemented=[2272]
nofork-new2 settled correct=True cps=1934.0 p50/95/99=112/339/685 fork_conf=0/0 close_med/max=1345/4335 rounds=[[0, '0'], [1, '0'], [2, '0']] decided=[0, 1, 2] uncemented=[0]
nofork-base2 settled correct=True cps=1958.4 p50/95/99=101/282/549 fork_conf=0/0 close_med/max=1533/2544 rounds=[[0, '0'], [1, '0'], [2, '0']] decided=[0, 1, 2] uncemented=[0]
```

## fix
```
fork5byz1-new settled correct=True cps=1435.6 p50/95/99=138/1023/1167 fork_conf=806/2248 close_med/max=2709/11900 rounds=[[0, '0'], [1, '2'], [2, '1'], [3, '0'], [4, '0']] decided=[0, 1, 2, 3, 4] uncemented=[5]
fork5-new settled correct=True cps=1743.9 p50/95/99=182/836/3104 fork_conf=839/2263 close_med/max=2769.0/8927 rounds=[[0, '0'], [1, '1'], [2, '0'], [3, '0']] decided=[0, 1, 2, 3] uncemented=[0]
fork5-new-b settled correct=True cps=1670.6 p50/95/99=173/1441/2156 fork_conf=1486/2344 close_med/max=3169.5/9352 rounds=[[0, '0'], [1, '0'], [2, '0'], [3, '0']] decided=[0, 1, 2, 3] uncemented=[0]
```
