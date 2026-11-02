# my-q-cli (`myq`)

Open, close and check your own myQ garage doors from the command line and from
MCP clients such as Claude. Unofficial; not affiliated with Chamberlain Group or
myQ.

Conforms to the [`piekstra-cli` spec v1](https://github.com/piekstra/cli-common).

> **Status: v0.1.0, not yet working end to end against a real account.** myQ
> accepted a real email and password and then asked for a one-time code. The code
> step is written and tested only against a hand-written page, so no sign-in has
> finished, no account or door has been read, and **no door has been moved by this
> tool**. See [`docs/api.md`](docs/api.md) for what is verified and what is not.

## Install

```console
$ cargo install --git https://github.com/piekstra/my-q-cli
```

or from a clone, which also signs the binary so macOS keychain grants survive a
reinstall:

```console
$ make install
```

## Sign in

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
| `myq doors list` | Every door and its state |
| `myq doors get <door>` | One door, by name, serial number or a unique part of its name |
| `myq doors open <door>` | Open it, then wait for a read-back to show it open |
| `myq doors close <door>` | Close it, then wait for a read-back to show it closed |
| `myq devices list` | Every device on the account |
| `myq accounts list`, `accounts use <account>` | The accounts this login can act on |
| `myq hub status --host <address>` | The opener's Wi-Fi status, read directly over the home network |
| `myq api <METHOD> <PATH>` | Raw API passthrough; anything but GET asks first |
| `myq mcp` | Door tools for MCP clients, over stdio |
| `myq auth login\|status\|logout\|set-credential` | The stored credential |
| `myq config`, `info`, `completions`, `self-update` | The family's standard commands |

Every command takes `--json` and emits one document tagged with a `schema`. Exit
codes: 0 ok, 2 usage, 3 auth, 4 not found, 5 upstream, 6 confirmation required.

## Moving a door

`doors open` and `doors close` ask first. Without a terminal they exit 6 unless given
`--force`. A door reference that matches more than one door is an error, never a first
pick. The command reports success only when a read-back shows the requested state:
`--timeout` sets how long it waits (60 seconds by default) and `--no-wait` sends the
command and reads once.

Opening a garage door exposes the house, and closing one can hit whatever is under it.
Make sure you can see the door, or that you mean it.

## Use with Claude

The tools are `myq_doors`, `myq_door_open` and `myq_door_close`. Sign in once in a
terminal first: a tool never signs in, and it renews only with the refresh token.

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
