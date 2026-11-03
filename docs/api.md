# The myQ API, as the Android app uses it

myQ publishes no API. Everything here comes from the owner's own copy of the Android app
(version 5.322) or from running this client against the owner's own account.

| Label | Means |
| --- | --- |
| **APK** | Read from the decompiled app and its `assets/myq-config.json`; not yet exercised |
| **Live** | Observed from the real service |
| **Assumed** | Neither: a guess this client makes, to be checked against a live run |

## Hosts

`https://<service>.myq-cloud.com`, with the API version `v6.0` in the path (**APK**).

| Service | Used for |
| --- | --- |
| `partner-identity` | Sign-in and tokens |
| `accounts` | The accounts a login can act on |
| `devices` | Devices and their state |
| `account-devices-gdo` | Door commands |

## Signing in

An OAuth authorization-code flow with PKCE, for a public client with no secret: client
id `ANDROID_CGI_MYQ`, scope `MyQ_Residential offline_access`, redirect
`com.myqops://android` (**APK**).

1. `GET /connect/authorize` with those values, `response_type=code`,
   `code_challenge_method=S256`, `ui_locales` and `prompt=login` (**APK**).
   `acr_values=unified_flow:v1  brand:myq` is also sent; the two spaces are the app's
   format string with an empty part, and the brand value is **Assumed**, though myQ
   accepted it (**Live**).
2. That redirects to `/Account/Login`, a server-rendered page behind Cloudflare. Its form
   posts `Email`, `Password`, `ReturnUrl`, `Brand`, `UnifiedFlowRequested` and
   `__RequestVerificationToken` to `/Account/LoginWithEmail` (**Live**).
3. The right password redirects to `/AccountMfa/VerifyOtp`, and reaching that page sends
   the code, by text message on the account tried (**Live**). Its form posts the 6-digit
   `Otp` with the hidden `SelectedMfaMethod` (`Sms`), `IsSmsMethodEnabled`, `MaskedEmail`,
   `MaskedPhoneNumber`, `__Invariant` (`Otp`) and `__RequestVerificationToken` back to
   `/AccountMfa/VerifyOtp` (**Live**). The page's script only validates the input, so this
   is a plain form post (**Live**). Links offer "Send the code via email instead" and "Send
   me a new code"; their query strings have not been read, and this client uses neither.
4. The right code redirects to `com.myqops://android?code=…` (**Live**).
5. `POST /connect/token` with `client_id`, `scope`, `grant_type=authorization_code`,
   `code`, `redirect_uri` and `code_verifier` (**APK**), plus the attestation described
   under Traps. **This step is where sign-in stops** (**Live**).

Renewal is `POST /connect/token` with `client_id`, `scope`, `redirect_uri`,
`grant_type=refresh_token` and `refresh_token` (**APK**). The app files `401.122` under
renewal failures too, so renewal is behind the same gate (**APK**). Whether refresh tokens
rotate is unknown.

## Reading

| Request | Answers | Source |
| --- | --- | --- |
| `GET accounts…/api/v6.0/accounts` | The login's accounts | **APK** |
| `GET devices…/api/v6.0/accounts/{id}/devices` | Devices | **APK** |

A device's `state` carries `door_state` (the app defaults to `unknown`), `online`,
`last_update`, `lamp_state`, `is_unattended_open_allowed` and
`is_unattended_close_allowed` (**APK**). The response's outer shape and values such as
`opening` are **Assumed**, so the fixtures for them are hand-written. A door is any device
whose state has a `door_state`.

## Door commands

`PUT account-devices-gdo…/api/v6.0/accounts/{id}/door_openers/{serial}/open` or `/close`,
with no body (**APK**). **This client has never sent one.** The app also has
`lamps/{serial}/on|off`, which this client does not use.

## The opener on the home network

The opener's Wi-Fi module serves a read-only status document at `http://<address>/sys`
with no sign-in (**Live**). It exposes no door control. `myq hub status` reads only that.

## Traps

- **Firebase App Check blocks this client.** The app sends a `Firebase-AppCheck-Token`
  header on the code exchange (**APK**). Without one, `/connect/token` answers HTTP 401 with
  `{"code":"401.122","description":"The application is unauthorized"}`, to a real code and
  to a dummy one alike (**Live**), and the app labels that code `REFRESH_APP_CHECK_FAILED`
  (**APK**). The sign-in page, password and text-message code all work without it, so the
  failure appears only after a code has been spent. `begin` therefore checks the gate with a
  dummy code before posting the password, and goes on only if myQ rejects that code with a
  400 and an `error`. What an open gate answers is **Assumed**, since it has only been seen
  closed. This client does not fake attestation.
- **A password post may send a code.** Retrying a sign-in can spend codes and walk the
  account toward a lockout, so nothing here retries one, and a session that ends on a login
  needing a code asks for a person instead of posting the password.
- **A code belongs to its session.** A fresh sign-in presenting an older code is refused,
  which reads like a typo. That is why the sign-in is parked and `--code` resumes it.
- **A page answers 200 either way.** A refused password or code re-renders a form, so
  success is the redirect carrying the authorization code, never a status (**Assumed** for
  refusals, which have not been seen live).
- **A command's status is not the door's state.** Success is a read-back showing the
  requested state. A door takes about 20 seconds to travel, and a close sounds its alarm
  first (**Assumed**).
- **A second command mid-travel.** Some openers take one as stop or reverse. What myQ does
  is unknown, so this client sends none while a door reads `opening` or `closing` toward the
  requested state, and the MCP tools do not claim to be idempotent.
- **A backslash ends a host.** URL parsers treat `\\` as `/` for https, so a hand-written host
  check can pass a URL that goes elsewhere. `api` checks with the parser the request uses.
- **Pinned hosts.** The app refused a proxy certificate for one media host while capturing
  (**Live**); the API hosts' pinning has not been established.

## Capturing traffic

`scripts/capture/` runs the app in an emulator through a redacting mitmproxy addon and
writes outside the repo. It is needed only when a live run shows a gap that the app's code
cannot explain.
