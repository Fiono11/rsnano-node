# Twelve-seat six-region matrix, 2000 blocks/s × 45,000 (Nov 16-19 2026, complete)

Twelve seats (f = p = 2) across six GCP regions, n2-standard-4 Spot VMs, set `final-wan-2000-n12`
(`tools/rai/run_sweep.py`), arms RAI, pause and Nano (develop), three repetitions. The committee-change
(`reconf`) cells add two Toronto standbys (PR12, PR13) that take the weight of both Montreal members
(PR2, PR8) in epoch 0. The three Byzantine committee-change cells have two repetitions (cut on Nov 18 for
credit): 99 runs in all. 76 ran by Nov 18; the matrix then resumed on a rebuilt fleet (see Builds) for
the rest.

## Builds

| Runs | Node build |
|---|---|
| `nofork`, `fork5-grace`, the smoke runs, pause and Nano rep 1 of `fork10-grace` | `baba6248c` (frozen build) |
| every other run | `e4601219e` (read-lock checks for no-op fork, activation and instance calls), sha256 `3acfd65f…` |

Runs from Nov 18 (resumed on a new account) ran on a rebuilt fleet: same names, zones, machine type and Spot
model, split over two peered projects (32-vCPU quota each), PR7-PR13 at new internal addresses. Binaries
rebuilt from the same node code (`e4601219e`) and develop `afe63ab1c` with rustc 1.100.0: rai sha256
`a0181c9b…`, develop `b0b706e6…`. A same-seed Nano `nofork-reconf` rep 1 on the new fleet
(`gcp-n12-newfleet-check-2026-11-18`): 1946/s, p50 525 ms (old 1937/s, 523 ms); p95 1023 ms vs 620 ms, all of
it in the first ~5 s of load (from second 6 on p95 535-622 vs 550-593 ms).

The develop (Nano) binary is the same for both. `fork10-grace` RAI reps 1 and 2 are the fix's test runs from
`gcp-n12-locks-2026-11-16` (same seeds, settings and binary; see `ADOPTED.txt`).

## Limits

`--timeout 480 --stall-abort 90 --drain-limit 15 --settle-timeout 600`; `--fork-grace-stall 30` until
Nov 17 13:24, then **180** (user's choice) after `fork10-grace-byznode` pause rep 1 was cut by the 30 s rule
during an 85 s epoch close with all nodes still progressing. Every run finished before the change settled well
inside 30 s, so the longer limit would not have changed any of them.

Nov 18 (user's choice, resumed fleet): once no position is open, the 180 s now counts from the last epoch
decided on every node, not from the moment the last position resolved, within the same 600 s cap
(`fork_grace.catch_ups` in `result.json` counts the restarts). The `fork10-grace-reconf` RAI rep 3 rerun had
been cut at 12:59 with PR2 (a departing Montreal member) 8 epochs behind and still deciding one about every
25 s; all blocks cemented everywhere and values agreed. A run that settled under the old rule ends the same way.

## Results (non-fork goodput blocks/s, latency ms unless s, longest close)

| Cell | RAI | pause | Nano |
|---|---|---|---|
| nofork | 3/3 settled; 1758-1937, p50 417-433, p95 669-787, close ≤ 4 s | 3/3 settled; 1632-1811, p50 482-880, p95 3394-5089, close ≤ 6 s | 3/3 safe; 1929-1937, p50 513-518, p95 581-584 |
| fork5-grace | 3/3 settled; 1779-1832, p50 432-457, p95 861-1324, close ≤ 6 s | 3/3 settled; 1100-1239, p50 2233-2721, p95 14.4-19.1 s, close ≤ 15 s | 3/3 safe; 1756-1773, p50 514-519, p95 574-580 |
| fork10-grace | 3/3 settled; 264-946, p50 595-662, p95 6.9-51.1 s, close ≤ 83 s | 3/3 settled; 1141-1154, p50 2376-5061, p95 12.1-13.9 s, close ≤ 13 s | 3/3 safe; 467-759, p50 520-531, p95 935-1277 |
| nofork-byznode | 3/3 settled; 1928-1937, p50 439-458, p95 733-806, close ≤ 6 s | 3/3 settled; 1510-1632, p50 1150-4877, p95 5.3-10.9 s, close ≤ 10 s | 3/3 safe; 1937, p50 513-526, p95 571-591 |
| fork5-grace-byznode | 3/3 settled; 1389-1771, p50 481-542, p95 1255-8006, close ≤ 15 s | 3/3 settled; 983-1375, p50 4307-5826, p95 12.3-17.1 s, close ≤ 16 s | 3/3 safe; 1761-1770, p50 512-524, p95 569-660 |
| fork10-grace-byznode | 3/3 settled; 154-1450, p50 626-7225, p95 3.0-186.1 s, close ≤ 117 s | 3/3 settled; 542-768, p50 4954-6032, p95 24.4-34.6 s, close ≤ 58 s | 3/3 safe; 570-972, p50 518-522, p95 834-1098 |
| nofork-reconf | 3/3 settled; 432-1880, p50 509-722, p95 1263-16.1 s, close ≤ 46 s | 3/3 settled; 801-1662, p50 2936-4818, p95 6.9-25.4 s, close ≤ 23 s | 3/3 safe; 1929-1937, p50 513-523, p95 606-636 |
| fork5-grace-reconf | 3/3 settled; 414-1648 (rep 1 no client figures, see below), p95 3.8-81.7 s, close ≤ 202 s | 3/3 settled; 739-1165, p50 2928-4156, p95 16.5-22.5 s, close ≤ 29 s | 3/3 safe; 1756-1771, p50 522-536, p95 645-867 |
| fork10-grace-reconf | 3/3 settled; 73-1185, p50 813-1134, p95 9.6-135.0 s, close ≤ 188 s | 3/3 settled; 510-1081, p50 2834-7619, p95 17.0-55.8 s, close ≤ 50 s | 3/3 safe; 793-1125, p50 519-523, p95 2009-2974 |
| nofork-reconf-byznode | 2/2 settled; 1912-1929, p50 448-494, p95 743-804, close ≤ 7 s | 2/2 settled; 1093-1525, p50 978-6241, p95 8.7-17.9 s, close ≤ 12 s | 2/2 safe; 1928-1929, p50 515-518, p95 600-601 |
| fork5-grace-reconf-byznode | 2/2 settled; 93-1276, p50 538-7655, p95 4.6-135.5 s, close ≤ 230 s | 2/2 settled; 150-700, p50 3912-9244, p95 9.9-209.0 s, close ≤ 162 s | 2/2 safe; 1772-1799, p50 511-525, p95 636-677 |
| fork10-grace-reconf-byznode | 2/2 settled; 350-905, p50 1029-1818, p95 14.3-54.7 s, close ≤ 145 s | 2/2 settled; 459-621, p50 7671-8435, p95 39.6-58.6 s, close ≤ 74 s | 2/2 safe; 1109-1161, p50 523-529, p95 1745-2052 |

Every RAI and pause run listed settled with agreeing ledgers; every Nano run held the same blocks with agreeing
values (its forks do not terminate). `fork5-grace-reconf` RAI rep 1 collapsed under load: 27,206 of 45,000
confirmed at the drain limit, so the client wrote no latency figures; it settled afterwards.

## Reruns and set-aside runs (`set-aside/`)

Every original is kept under `set-aside/` (result gzipped, summary, host file).

- `nofork-byznode` RAI rep 2: outlier (362/s, p95 67 s, close 89 s) rerun on user request: 1937/s, p95 806.
- `fork5-grace-byznode` pause rep 3: settled, but the client's publish sockets to PR7-PR11 blocked and closed,
  so it wrote no figures; rerun: 1375/s.
- `nofork-reconf` Nano rep 2: PR1 held 48,887 of 49,049 blocks (values agree, nothing conflicting) after the
  client's socket to PR1 blocked; rerun: safe, 1929/s.
- `fork10-grace-byznode` pause rep 1: cut by the 30 s fork-grace stall rule (see Limits); rerun under 180 s.
- `fork10-grace-reconf` RAI rep 2: failed — the client stalled at 35,493 at the drain limit, closes to 139 s with
  up to 4 rounds, epoch 19 not decided on every node when the 600 s fork grace ran out; values agree everywhere.
  Its rerun on the Nov 18 fleet settled: 1185/s, p95 9.6 s.
- `*.partial-*`: runs a halt or preemption interrupted (Iowa PR1/PR7 at 14:21 and Montreal PR2 at 15:35 on Nov 17,
  planned halts); their reruns are in the matrix.
- `*.failed-*` before Nov 17: the frozen-build `fork10-grace` RAI rep 2, `fork5-grace` pause rep 3, the first
  smoke attempt.

The fleet was all Spot: preempted VMs were restarted (no replacement was needed) and the run in progress rerun.

## Why RAI slows at 10% forks and after the committee change

Some seats (pr2, pr3, pr4, pr7, pr8, pr10) overload on vote processing: the elections lock (apply_vote holds the
write lock 25-45% of the time), vote queues at their caps (network 2816-3744, evidence 8192), 60-320k votes
dropped per node, CPU 2.3-3.4 of 4 cores. An overloaded seat signs its epoch report on time but serves it late.
A close needs reports of 10 of the 12 current members (N - f), so three slow members hold it.

The committee change makes this worse (`fork5-grace-reconf` RAI rep 2, `close_breakdown.py`): closes take 1-3 s in
epochs 0-2, 13 s in epoch 3 (first epoch of the new committee) and 124-206 s in epoch 5. In epoch 5 PR0 held 12
reports but usable weight 9 of the 10 needed: the departed members' (PR2, PR8) reports carry no weight, and PR4
and PR7 (both saturated, 160-320k votes dropped) reached PR0 88 s late although both froze their reports within
11 s. Median freeze rises from 0.1 s to 4-23 s as each late close delays the next. Not a correctness fault: the
known overload with one seat less of slack. A fix needs the vote recording out of the exclusive lock.
