# Claude Code Entry Point

Read `AGENTS.md` first; it is authoritative for every agent. Keep this file limited to Claude-specific tool mechanics and never contradict `AGENTS.md`.

- A foreground `sleep` is blocked. Wait on long jobs with `run_in_background` or Monitor, not `sleep` and poll.
- A command that hits its timeout keeps running in the background; check for leftover processes (`pgrep -x`, never `pgrep -f` on your own command text) before starting another test.
