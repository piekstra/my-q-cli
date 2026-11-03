# Security

`myq` can open your garage door, so treat its credentials like a house key.

## What it stores

| What | Where |
| --- | --- |
| Email, password, tokens, a sign-in waiting on its code | One item in the OS keychain, service `piekstra.myq` |
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
  rest, in the opposite state, and nothing taps twice.
- A door command succeeds only when a read-back shows the requested state.
- `api` sends the token only to `https` URLs on `myq-cloud.com`.
- The MCP door tools are marked destructive, so a client can ask before each call. `myq_doors` only reads;
  bringing the app forward is `myq_app_open`, marked as a write. A tool never prompts and uses only the
  phone, never the stored password.
- A stored password that myQ refused, or answers with a one-time code, is not posted again until the next
  `auth login`. More posts could only send more codes or walk the account toward a lockout.

## The phone

USB or wireless debugging lets the computer you allow control the phone, so allow only computers you
trust. `myq` runs `uiautomator`, `cmd package resolve-activity`, `am start` and `input tap` through `adb`.
It reads the foreground window, and a window from any other app is dropped after its package name. It
stores nothing from the phone.

## The opener's local status page

`myq hub status` sends one GET to `http://<address>/sys` on your home network, in plain HTTP with no
credentials, and changes nothing.

## Reporting

Report a vulnerability through GitHub's private vulnerability reporting on this repository, not in a
public issue. Do not include credentials, tokens or captures in a report.
