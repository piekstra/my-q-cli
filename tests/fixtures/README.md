# Test fixtures

The offline tests run the sign-in flow, the door logic and the MCP tools against these files.

## Where each file came from

Written on 2026-11-02. The hand-written files follow field names in the decompiled app or on a live page,
so their real shape is unverified. Replace each with a scrubbed capture once a live read shows it.

| File | Source |
| --- | --- |
| `login_page.html` | The live sign-in page's form, reduced and scrubbed |
| `hub_sys.json` | A live opener's status document, scrubbed |
| `code_step.html` | Hand-written from the input names on the live page; its form markup is assumed |
| `accounts.json`, `devices.json`, `token.json` | Hand-written from the app's field names |

## Scrubbing policy

Structure is preserved: keys, nesting, types, null versus absent. Every identifying value is an obvious
dummy, and `tests/repo_clean.rs` rejects home-network addresses, MAC addresses outside the documentation
block, email addresses outside `example.*`, and credential shapes in every tracked file.

| Kind | Dummy |
| --- | --- |
| Account ids | `00000000-0000-0000-0000-00000000000N` |
| Serial numbers | `DUMMYSERIALnn`, `DUMMYHUBnn` |
| Door names | `Dummy Left Door`, `Dummy Right Door` |
| Tokens | `dummy-access-token`, `dummy-refresh-token` |
| Addresses | `192.0.2.0/24` (RFC 5737), `00-00-5E-00-53-xx` (RFC 7042) |

Never commit a raw capture to scrub later. A myQ response ties a person's account to their home's
devices, and a token in one opens their garage.
