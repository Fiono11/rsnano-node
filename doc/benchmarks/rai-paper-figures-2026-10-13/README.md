# Paper figures, Oct 13 2026

Data of the evaluation's three figures (RAI_LaTeX/sections/eval-figures.tex):
`tools/rai/run_sweep.py OUT --set paper` at 1,000 blocks/s, one run per
scenario and system, build 16b530a7a (pause-transition carry and inherited-lock
continuation fixes).

    python3 tools/rai/run_sweep.py doc/benchmarks/rai-paper-figures-2026-10-13 --set paper \
      --arm rai=target/release --arm pause=target/release:--epoch-transition=pause \
      --arm develop=<develop bin dir>:--no-epochs:--timeout=240:--settle-timeout=45
    python3 RAI_LaTeX/tools/paper_figures.py doc/benchmarks/rai-paper-figures-2026-10-13 \
      RAI_LaTeX/sections/eval-data/paper

Scenarios: nofork, fork5-grace, fork10-grace, fork5-byzlead-grace (Byzantine
validator leads the first round of every close). Fork runs keep the nodes up
after settling until no block is uncemented anywhere, or none has been
cemented for 45 s (600 s cap); `open_trace` in result.json records the
uncemented count over wall-clock time.

develop (Nano) fork runs report `failed`: its validators end with different
cemented counts (fork5 616/617, fork10 8 to 81), which the harness's
correctness check rejects. The fork5-grace pause run waited about 10 minutes
for a quiet host before starting; the run itself is performance-eligible.
