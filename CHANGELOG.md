# Changelog

## 0.2.1 — 2026-11-04

- `auth logout --forget` says what it does not end whether or not the keychain held a connector token: a server on `$MYQ_MCP_TOKEN` never had an item to remove.
- cli-common v0.29.0, up from v0.25.0, which brings the Windows config and self-update fixes.

## 0.2.0 — 2026-11-03

- A late door command sends nothing. `myq_door_open`, `myq_door_close` and `myq_app_open` check the
  time right before each send to the phone, on the monotonic and the wall clock. At 30 seconds or more
  after the call began, they send nothing and say so, so a stalled `adb` or a Mac asleep through one
  cannot move a door after whoever asked has stopped waiting. The read-back stops at 85 seconds, so
  the slowest answer still arrives inside the 120 seconds `--http` waits. The CLI's own commands are
  unchanged.
- One door command at a time. A command on the phone holds a lock from its first read to its last, and
  a second one, from a terminal or a server, is refused with nothing tapped, because the app's status
  lags its button and a tap on a moving door could stop or reverse it. `--no-wait` holds it three seconds
  longer, after its tap.
- `myq mcp --http --port <PORT>` serves the tools on 127.0.0.1 behind a bearer token, for Claude on the
  web and mobile through your own Cloudflare Worker and tunnel. It serves `myq_doors` alone unless
  started with `--allow-writes`, and **with that flag whoever holds the token can move your door**.
  `myq mcp --new-token` makes the token, and `auth logout --forget` removes it and says how to stop a
  server still running with its own copy. `self-update` reminds you to restart a running server.
- Moved to cli-common v0.25.0.
