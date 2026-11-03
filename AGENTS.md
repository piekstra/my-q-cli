# my-q-cli

The `myq` CLI and MCP server for a person's own myQ garage doors, on the family's
shared crates ([`cli-common`](https://github.com/piekstra/cli-common), spec
`piekstra-cli/1`). `make verify` is the gate.

## Rules

- **Never open or close a real door without the owner's go-ahead at that moment.** It
  physically moves and exposes the house. A previous approval does not carry over.
- **API facts come only from the myQ Android app**: its APK or its captured traffic.
  Do not read other myQ integrations, and do not copy their endpoints or flows. Label
  each fact's source in `docs/api.md`.
- **No attestation forgery.** myQ requires a Firebase App Check token on the token
  exchange and answers `401.122` without one, so sign-in cannot finish. Do not fake or
  replay that header, send the app's failure marker, spoof a TLS fingerprint, or try
  other clients or paths around the gate. Report the block instead.
- **Every password post can cost a one-time code**, and failed posts can lock the
  account. Never put a sign-in in a loop or a retry. After a failure, diagnose first.
- **The repo carries no personal data.** Runtime output may; tracked files may not.
  `tests/repo_clean.rs` enforces shapes. Never commit a raw capture or an APK.
- **Tests are offline** and never touch a real keychain item. Set `MYQ_TEST_KEYCHAIN=1` with
  `MYQ_KEYCHAIN_SERVICE=<a throwaway name>` to run the two surface tests that read one.
- **No environment-variable credentials.** The family's spec puts them before the keychain. Here a session
  must keep rotated tokens and what myQ said about the password, or every process would post the password
  again. `MYQ_EMAIL` and `auth login --from-env` are sign-in inputs only.

## Layout

| File | Holds |
| --- | --- |
| `src/auth.rs` | The sign-in's requests: PKCE, the form posts, the code step, tokens |
| `src/signin.rs` | `auth login`'s sequence: what is parked, stored, and refused |
| `src/client.rs` | API calls, session renewal, the door command and its read-back |
| `src/credential.rs` | The one keychain item, and the parked sign-in inside it |
| `src/door.rs` | DTOs, parsing of API responses, and door-reference resolution |
| `src/hub.rs` | The opener's local status page; read-only |
| `src/mcp.rs` | The MCP tools, over a `Source` seam |
| `src/main.rs` | The clap tree and exit-code mapping |
| `scripts/capture/` | Redacting mitmproxy capture of the app, from an emulator |
| `packaging/mcpb/` | The Claude Desktop extension manifest |

## Signing in as an agent

Read credentials from 1Password by reference, never by printing them:

```console
$ myq auth login --email-op op://VAULT/ITEM/username --op op://VAULT/ITEM/password --non-interactive
```

That exits 3 with the sign-in parked when myQ wants a code. Get the code from the owner
or, when myQ emails it, their mailbox, and finish with `printf '%s' "$CODE" | myq auth login --code -`.

If `op` or a keychain read hangs or times out, the owner was not there to approve it.
Stop calling it, say what you need, and wait.

## Verifying a change

1. `make verify`.
2. `make install`, because a stale `~/.cargo/bin/myq` keeps serving old behaviour.
3. Drive the installed binary, including `--json` and the error paths. Reads are fine;
   door commands need the rule above.

Replace a hand-written fixture with a scrubbed capture whenever a live read shows its
real shape, and update `docs/api.md` in the same change.
