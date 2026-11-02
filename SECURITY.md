# Security

`myq` can open your garage door, so treat its credentials like a house key.

## What it stores

| What | Where |
| --- | --- |
| Email, password, tokens, a sign-in waiting on its code | One item in the OS keychain, service `piekstra.myq` |
| Email, account id, hub address, 1Password references | `~/.config/myq/config.json` (no secrets) |

Secrets enter through a no-echo prompt, `--stdin`, `--from-env`, or `--op <reference>`, never as a
command-line argument. `MYQ_EMAIL` and `MYQ_PASSWORD` override the keychain and are never written to it.

## Safeguards

- `doors open`, `doors close` and any `api` call but GET ask first, and exit 6 without `--force` when
  they cannot ask. The check runs before any keychain or network work.
- A door reference that matches more than one door is an error, never a first pick.
- A door command succeeds only when a read-back shows the requested state.
- `api` sends the token only to `https` URLs on `myq-cloud.com`.
- The MCP door tools are marked destructive, so a client can ask before each call. A tool never prompts
  and never posts the password; it renews only with the refresh token.
- A session that ends on a login needing a one-time code asks for a person. It does not post the password,
  which could make myQ send more codes.

## The opener's local status page

`myq hub status` sends one GET to `http://<address>/sys` on your home network, in plain HTTP with no
credentials, and changes nothing.

## Reporting

Report a vulnerability through GitHub's private vulnerability reporting on this repository, not in a
public issue. Do not include credentials, tokens or captures in a report.
