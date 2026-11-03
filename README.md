# my-q-cli (`myq`)

Open, close and check your own myQ garage doors from the command line and from
MCP clients such as Claude. Unofficial; not affiliated with Chamberlain Group or
myQ.

Conforms to the [`piekstra-cli` spec v1](https://github.com/piekstra/cli-common).

> **Status: v0.1.0. Reading a door works; moving one is untested.** myQ's cloud API
> refuses this tool: after a real password and one-time code, its gateway answers the
> token exchange with `401.122`, because the request has no Firebase App Check
> attestation. Only myQ's own app can provide one, and this tool does not fake it. So
> `myq doors` operates the real app on your Android phone over `adb`, and `doors list`
> has read a real door that way. **No command of this tool has moved a door yet**:
> `doors open` and `doors close` are tested only against a simulated phone. See
> [`docs/api.md`](docs/api.md) for what is verified and what is not.

## Install

```console
$ cargo install --git https://github.com/piekstra/my-q-cli
```

or from a clone, which also signs the binary so macOS keychain grants survive a
reinstall:

```console
$ make install
```

## Set up your phone

`myq doors` and the MCP tools need:

- The myQ app on an Android phone, signed in, with your door on its Home tab.
- `adb` from Android's platform-tools on this computer. `myq` looks in `ANDROID_HOME`,
  `ANDROID_SDK_ROOT`, the usual SDK folders and then `PATH`; `MYQ_ADB` names it directly.
- USB debugging on (Settings > Developer options) with this computer allowed, or wireless
  debugging. If more than one device is connected, `ANDROID_SERIAL` names the phone.
- The phone on and unlocked when a command runs.

The CLI's `doors` commands and the MCP open and close tools bring the app to the phone's
screen if another app is in front, and leave it there. The MCP `myq_doors` tool never does: it
reports that no door is showing, and `myq_app_open` brings the app forward.

## Sign in to the cloud API

myQ's App Check gate blocks this today, so nothing above needs it. While the gate is closed,
`auth login` stops before posting the password and spends no code.

```console
$ myq auth login --email you@example.com
myQ password:
a verification code was sent to y***@example.com
Verification code:
```

Passwords come from the no-echo prompt, `--stdin`, `--from-env <VAR>`, or 1Password
with `--op op://VAULT/ITEM/password` (and `--email-op` for the email). They are never
a command-line argument.

myQ can ask for a one-time code. Without a terminal, `auth login` exits 3 and keeps
the sign-in waiting in the keychain for 20 minutes. Finish that same sign-in with:

```console
$ myq auth login --code 123456
$ printf '%s' "$CODE" | myq auth login --code -
```

A code only counts against the sign-in that asked for it, so run a fresh
`auth login` only when you want a new code.

The session lives in the OS keychain under the service `piekstra.myq`. When it ends
on a login that needs a code, or myQ refuses the stored password, commands exit 3 and
ask you to sign in again. They do not post that password again, because more posts
could only send more codes or lock the account.

## Commands

| Command | What it does |
| --- | --- |
| `myq doors list` | Every door on the app's screen, and its state |
| `myq doors get <door>` | One door, by name or a unique part of it |
| `myq doors open <door>` | Open it, then wait for a read-back to show it open |
| `myq doors close <door>` | Close it, then wait for a read-back to show it closed |
| `myq devices list` | Every device on the account |
| `myq accounts list`, `accounts use <account>` | The accounts this login can act on |
| `myq hub status --host <address>` | The opener's Wi-Fi status, read directly over the home network |
| `myq api <METHOD> <PATH>` | Raw API passthrough; anything but GET asks first |
| `myq mcp` | Door tools for MCP clients, over stdio |
| `myq auth login\|status\|logout\|set-credential` | The stored credential |
| `myq config`, `info`, `completions`, `self-update` | The family's standard commands |

`devices`, `accounts`, `api`, `auth login` and `doors --cloud` use the cloud API, which
myQ's gate blocks today. `--account` and `MYQ_ACCOUNT` apply only to those: `doors` without
`--cloud` and `mcp` refuse them, because the phone acts on the account its app is signed in to.

Through the phone, a door document carries `name`, `state` and `status` (the app's wording),
plus `online: false` for an offline door. `serial` and `last_update` appear only with `--cloud`.

Every command takes `--json` and emits one document tagged with a `schema`. Exit
codes: 0 ok, 2 usage, 3 auth, 4 not found, 5 upstream, 6 confirmation required.

## Moving a door

`doors open` and `doors close` ask first. Without a terminal they exit 6 unless given
`--force`. A door reference that matches more than one door is an error, never a first
pick. The command reports success only when a read-back shows the requested state:
`--timeout` sets how long it waits (60 seconds by default) and `--no-wait` taps and reads
once.

The door button is tapped only when the screen read just before it shows exactly one door,
at rest, in the opposite state. A door that is moving, offline or in a state `myq` does not
recognise gets no tap, because a tap mid-travel could stop or reverse it. Nothing taps twice.

Opening a garage door exposes the house, and closing one can hit whatever is under it.
Make sure you can see the door, or that you mean it.

## Use with Claude

The tools are `myq_doors`, `myq_app_open`, `myq_door_open` and `myq_door_close`. They work
through the phone, so [set it up](#set-up-your-phone) first, on the computer running the
client. `myq_doors` only reads, so if the app is not showing its doors, `myq_app_open` brings
it forward.

| Client | How |
| --- | --- |
| Claude Code | `claude mcp add myq -- myq mcp` |
| Claude Desktop | Install `myq-<version>.mcpb` from a release (none published yet) |
| claude.ai on the web and mobile | Not built yet |

## Documentation

- [`docs/api.md`](docs/api.md): the API, where each fact came from, and its traps.
- [`SECURITY.md`](SECURITY.md): what is stored and how to report a vulnerability.
- [`CONTRIBUTING.md`](CONTRIBUTING.md): the gate, and where API facts may come from.

## License

MIT or Apache-2.0, at your option.
