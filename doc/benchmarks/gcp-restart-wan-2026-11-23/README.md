# Restarts across six regions on the unpatched matrix build, Nov 23 2026

Same set and flags as `gcp-restart-wan-fix-2026-11-23`, on the twelve-seat matrix build (`a0181c9b`). Reps 1-2 of fork10-restart settled (1,920 and 1,911 blocks/s, p95 567 and 566 ms; the restarted PR2 caught up after 64-73 s and 28 s). Rep 3 did not settle: PR2 never obtained epoch 2's checkpoint (see the fix set's README); the sweep stopped there. Safety held in all three (0 conflicting signatures).
