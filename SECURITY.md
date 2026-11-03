# Security

`myq` can open your garage door, so treat its credentials like a house key.

## What it stores

| What | Where |
| --- | --- |
| Email, password, tokens, a sign-in waiting on its code | One item in the OS keychain, service `piekstra.myq` |
| The bearer token of `myq mcp --http`, once `myq mcp --new-token` has made it | The item `mcp-http-token` in that service, or `MYQ_MCP_TOKEN` |
| Email, account id, hub address, 1Password references | `~/.config/myq/config.json` (no secrets) |

Secrets enter through a no-echo prompt, `--stdin`, `--from-env`, or `--op <reference>`, never as a
command-line argument. For the cloud API, environment variables are only sign-in inputs: `MYQ_EMAIL`, and
`auth login --from-env <VAR>`. The session always lives in the keychain. `MYQ_ADB`, `ANDROID_HOME`,
`ANDROID_SDK_ROOT` and `ANDROID_SERIAL` choose the `adb` program and device for [the phone](#the-phone) and
carry no secrets, though `MYQ_ADB` does name a program `myq` will run.

## Safeguards

- `doors open`, `doors close` and any `api` call but GET ask first, and exit 6 without `--force` when
  they cannot ask. The check runs before any phone, keychain or network work.
- A door reference that matches more than one door is an error, never a first pick.
- The phone's door button is tapped only when the screen read just before it shows exactly one door, at
  rest, in the opposite state, and no command taps twice.
- A door command on the phone holds a lock (`~/.config/myq/door.lock`) from its first read to its last. A
  second one, from any process, is refused with nothing tapped, because the app's status lags its button
  and a tap on a moving door could stop or reverse it.
- A door command succeeds only when a read-back shows the requested state.
- `api` sends the token only to `https` URLs on `myq-cloud.com`.
- The MCP door tools are marked destructive, so a client can ask before each call. `myq_doors` only reads;
  bringing the app forward is `myq_app_open`, marked as a write. A tool never prompts and uses only the
  phone, never the stored password.
- An MCP call sends nothing to the phone 30 seconds or more after it began, by the monotonic or the
  wall clock, and says so, because `--http` answers its caller at 120 seconds without cancelling the
  call. That covers the door's tap and bringing the app forward. The CLI's own commands have no such
  limit: someone is at the terminal.
- A stored password that myQ refused, or answers with a one-time code, is not posted again until the next
  `auth login`. More posts could only send more codes or walk the account toward a lockout.

## The phone

USB or wireless debugging lets the computer you allow control the phone, so allow only computers you
trust. `myq` runs `uiautomator`, `cmd package resolve-activity`, `am start` and `input tap` through `adb`.
It reads the foreground window, and a window from any other app is dropped after its package name. It
stores nothing from the phone.

## `myq mcp --http`

`myq mcp --http --port <PORT>` serves the tools for your own Cloudflare Worker, which reaches them
through your own Cloudflare Tunnel (cli-common's `connector/`). It binds 127.0.0.1 only, with no flag
for another address, and refuses to start without a bearer token of 32 or more characters, from
`MYQ_MCP_TOKEN` or the keychain item `mcp-http-token` (`piekstra.myq`), which `myq mcp --new-token`
makes from 256 random bits and replaces only with `--overwrite`. Every request must carry the token
(compared in constant time) and a `Host` of `127.0.0.1` or `localhost`, and must not carry an `Origin`;
there are no CORS headers.

It serves `myq_doors` alone, and a write is neither listed nor callable, unless started with
`--allow-writes`. **With that flag, whoever holds the token can move your door.** The approval a tool
asks for is the client's, and a client that ignores the annotation does not ask.

**Tool results also cross Cloudflare on this path**, through your tunnel and your Worker, on their way
to Claude, and the Worker holds a copy of the token as `HOME_MYQ_TOKEN`. Who holds what, and what a
leaked token reaches, is in the
[connector's threat model](https://github.com/piekstra/cli-common/tree/main/connector#threat-model).

`myq auth logout --forget` removes the stored credential and `mcp-http-token`. A server already running
keeps the token it started with and goes on answering until it is stopped, and the command says so,
because the door tools use the phone and not the session. A token it cannot remove still lets it clear
the session and the config, and it then says so and exits nonzero. Plain `auth logout` keeps the token.
To revoke the connector's access and keep serving, run `myq mcp --new-token --overwrite` and
restart the server (`launchctl kickstart -k gui/$(id -u)/com.piekstra.myq-mcp-http`); the old token
works until the restart. To stop serving, run `launchctl bootout
gui/$(id -u)/com.piekstra.myq-mcp-http` and stop the tunnel.

## The opener's local status page

`myq hub status` sends one GET to `http://<address>/sys` on your home network, in plain HTTP with no
credentials, and changes nothing.

## Reporting

Report a vulnerability through GitHub's private vulnerability reporting on this repository, not in a
public issue. Do not include credentials, tokens or captures in a report.
