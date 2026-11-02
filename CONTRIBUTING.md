# Contributing

`make verify` is the gate, and CI runs the same steps: `cargo fmt --check`, `clippy -D warnings`, the
tests, the capture addon's tests, and an offline smoke of the release binary.

- Tests are offline. No test may reach the network or a real keychain item.
- Test data that represents a wire format lives in `tests/fixtures/` and follows the scrubbing policy
  in `tests/fixtures/README.md`.
- Every API fact in `docs/api.md` comes from the myQ Android app: its APK or its captured traffic. Do
  not copy endpoints or flows from other myQ integrations.
- Never commit a raw capture or an APK. `scripts/capture/` writes everything outside the repo.
