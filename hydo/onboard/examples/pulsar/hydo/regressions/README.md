Saved minimal schedules, one `<name>.schedule` file each (`hydo save <explore dir>` writes them;
`hydo repro --regressions` replays them). Only `*.schedule` files are read. RESULTS.md names the
finding of each. A file whose first lines name build parameters (`# build: --set ...`) holds for
the image built with them: build it with those `--set` values before the replay.
