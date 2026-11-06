# Six-region matrix on the voter lock build, Nov 6 2026

45,000 blocks at 2000/s, 8 s epochs, 3 repetitions on the seeds of `gcp-matrix2-45k-2026-11-05`. Node sha 8a7c1763 for RAI and pause, 478d1750 for Nano; client 588f5eab for all. No vote audit.
Nano's static cells are not here: its node is unchanged and the weight change only affects setups with a standby, so they stay in `gcp-matrix2-45k-2026-11-05`.
All 36 RAI and 36 pause runs settled with correct ledgers. All 18 Nano runs are safe: every node holds the same blocks and the values agree; Nano does not terminate forks, so its `failed` fork runs end on that check.

Non-fork latency in ms, goodput in blocks/s, slowest close in s, ranges over the repetitions. "Gap" counts seconds inside the load in which the client published nothing: it waits for each block to be written to every node, so one blocked socket stops the load for all. Its cause is not established; no node had a queue backed up.

| arm | cell | settled | goodput | p50 | p95 | p99 | slowest close | gap s per run |
|---|---|---|---|---|---|---|---|---|
| rai | nofork | 3/3 | 1912-1962 | 373-405 | 468-525 | 570-591 | 3.8-4.0 | 0,0,0 |
| rai | nofork-byznode | 3/3 | 1596-1920 | 395-402 | 476-756 | 572-886 | 4.5-8.0 | 1,0,3 |
| rai | nofork-reconf | 3/3 | 1945-1946 | 385-391 | 547-561 | 652-724 | 4.2-4.4 | 0,0,0 |
| rai | nofork-reconf-byznode | 3/3 | 1668-1954 | 385-419 | 537-601 | 634-685 | 5.2-6.2 | 0,4,0 |
| rai | fork5-grace | 3/3 | 1857-1859 | 379-390 | 495-537 | 632-889 | 3.5-5.7 | 0,0,0 |
| rai | fork5-grace-byznode | 3/3 | 1825-1847 | 391-420 | 526-556 | 629-818 | 5.2-8.0 | 0,0,0 |
| rai | fork5-grace-reconf | 3/3 | 1726-1853 | 383-415 | 582-2005 | 809-2851 | 4.5-8.7 | 0,0,0 |
| rai | fork5-grace-reconf-byznode | 3/3 | 1734-1833 | 407-440 | 603-794 | 711-1053 | 6.3-7.8 | 0,1,2 |
| rai | fork10-grace | 3/3 | 1467-1755 | 399-414 | 564-653 | 751-837 | 3.8-4.2 | 4,0,0 |
| rai | fork10-grace-byznode | 3/3 | 1349-1747 | 395-485 | 599-994 | 736-1068 | 5.1-13.9 | 0,8,0 |
| rai | fork10-grace-reconf | 3/3 | 1699-1744 | 391-438 | 640-2261 | 805-3139 | 3.9-11.5 | 0,0,0 |
| rai | fork10-grace-reconf-byznode | 3/3 | 1709-1749 | 406-438 | 694-798 | 918-1210 | 8.9-9.6 | 0,0,0 |
| pause | nofork | 3/3 | 1552-1954 | 488-911 | 4663-6127 | 5346-6570 | 4.9-9.0 | 0,0,0 |
| pause | nofork-byznode | 3/3 | 1451-1574 | 1416-1686 | 5179-6865 | 5749-7761 | 5.1-7.6 | 2,2,1 |
| pause | nofork-reconf | 3/3 | 1674-1747 | 501-593 | 4592-4987 | 5107-5825 | 4.9-6.2 | 0,0,0 |
| pause | nofork-reconf-byznode | 3/3 | 1557-1608 | 1090-1730 | 5226-5622 | 5786-6477 | 5.4-6.3 | 1,1,1 |
| pause | fork5-grace | 3/3 | 1502-1828 | 546-726 | 4910-5649 | 5504-6080 | 4.9-6.8 | 0,0,0 |
| pause | fork5-grace-byznode | 3/3 | 825-1097 | 3043-5759 | 9694-15980 | 10309-16625 | 15.5-17.9 | 2,2,2 |
| pause | fork5-grace-reconf | 3/3 | 1244-1506 | 695-1489 | 5065-12203 | 6441-13069 | 7.2-13.7 | 0,0,0 |
| pause | fork5-grace-reconf-byznode | 3/3 | 992-1126 | 3108-4978 | 16003-21936 | 17455-22970 | 14.7-20.6 | 0,20,1 |
| pause | fork10-grace | 3/3 | 1174-1462 | 555-732 | 4927-12470 | 5456-13467 | 6.2-11.8 | 0,0,2 |
| pause | fork10-grace-byznode | 3/3 | 764-1408 | 1785-6480 | 9240-22537 | 9700-25491 | 15.6-24.9 | 5,22,5 |
| pause | fork10-grace-reconf | 3/3 | 1330-1396 | 603-1359 | 4635-7334 | 6818-8028 | 8.0-10.6 | 0,2,0 |
| pause | fork10-grace-reconf-byznode | 3/3 | 586-1055 | 2651-6780 | 12895-40243 | 14436-44169 | 14.1-38.6 | 4,38,32 |
| develop | nofork-reconf | 3/3 | 1904-1937 | 514-532 | 607-864 | 1495-1962 | - | 1,0,0 |
| develop | nofork-reconf-byznode | 3/3 | 1020-1937 | 537-538 | 625-1246 | 1509-1761 | - | 0,0,5 |
| develop | fork5-grace-reconf | 3/3 | 1218-1776 | 534-536 | 757-999 | 2347-3564 | - | 0,0,0 |
| develop | fork5-grace-reconf-byznode | 0/3 | 1165-1778 | 526-539 | 783-1098 | 1908-2128 | - | 0,0,2 |
| develop | fork10-grace-reconf | 1/3 | 480-1005 | 530-531 | 1883-2631 | 11124-50604 | - | 0,0,4 |
| develop | fork10-grace-reconf-byznode | 0/3 | 554-1081 | 534-536 | 1916-2117 | 11827-34695 | - | 0,1,0 |
