//! `auth login`, start to finish: what is asked of myQ, what is kept, and when.
//! Generic over the transport and the store so all of it runs offline.

use std::cell::RefCell;

use pk_cli_auth::otp::{CodeSent, OtpCode, OtpLogin, OtpTransport};
use pk_cli_core::transport::Transport;
use pk_cli_core::{block_on, CliError};

use crate::auth::{self, Begun, Pending, Pkce, Tokens};
use crate::client::unix_now;
use crate::credential::{Credentials, ParkedView, TokenStore};

/// How `auth login` may reach the person running it.
#[derive(Debug, Clone, Copy)]
pub struct Prompting {
    pub interactive: bool,
    pub quiet: bool,
}

/// The code step for the shared one-time-code flow. `begin` has already reached it,
/// because whether a code is needed at all is only known after the password.
struct CodeStep<'a, T> {
    transport: &'a T,
    reached: RefCell<Option<Pending>>,
}

impl<T: Transport> OtpTransport for CodeStep<'_, T> {
    type Session = Tokens;

    fn request_code(&self) -> Result<CodeSent, CliError> {
        let pending = self
            .reached
            .borrow_mut()
            .take()
            .ok_or_else(|| CliError::Other("no sign-in has reached its code step".into()))?;
        let in_flight = serde_json::to_string(&pending)
            .map_err(|e| CliError::Other(format!("parking the sign-in: {e}")))?;
        let mut sent = CodeSent::new(in_flight);
        sent.channel = pending.channel;
        sent.sent_to_hint = pending.sent_to_hint;
        Ok(sent)
    }

    fn redeem(&self, in_flight: &str, code: &OtpCode) -> Result<Tokens, CliError> {
        let pending: Pending = serde_json::from_str(in_flight).map_err(|_| {
            CliError::Auth(format!(
                "the waiting sign-in could not be read; run `{} auth login` for a new code",
                crate::BIN
            ))
        })?;
        block_on(auth::redeem(self.transport, &pending, code.expose()))
    }
}

/// Refuses to replace a stored session unless asked. A waiting sign-in or a stored
/// password is what a new sign-in is for, so neither is in the way of one.
pub fn check_overwrite(
    existing: Option<&Credentials>,
    no_verify: bool,
    overwrite: bool,
) -> Result<(), CliError> {
    let in_the_way = if no_verify {
        existing.is_some()
    } else {
        existing.is_some_and(Credentials::has_session)
    };
    if in_the_way && !overwrite {
        return Err(CliError::Usage(
            "a credential is already stored; pass --overwrite to replace it".into(),
        ));
    }
    Ok(())
}

/// How far a sign-in got, when myQ accepted the password.
#[derive(Debug)]
pub enum Started {
    Signed(Credentials),
    /// Stopped at the code step, usually parked for [`resume`]. This is how the
    /// command ends (exit 3 when it is waiting on a code).
    WaitingOnCode(CliError),
}

/// Sign in with the email and password. `Err` means myQ did not accept them, and
/// nothing was stored.
pub fn start<T: Transport, S: TokenStore>(
    transport: &T,
    store: &S,
    email: &str,
    password: &str,
    prompting: Prompting,
) -> Result<Started, CliError> {
    let seed = Credentials {
        email: email.to_string(),
        password: Some(password.to_string()),
        ..Credentials::default()
    };
    let pkce = Pkce::generate()?;
    match block_on(auth::begin(transport, email, password, &pkce))? {
        Begun::Signed(tokens) => keep_session(store, seed, tokens).map(Started::Signed),
        Begun::FailedAfterPost(err) => Err(err),
        Begun::Code(pending) => {
            let step = CodeStep {
                transport,
                reached: RefCell::new(Some(pending)),
            };
            let slot = ParkedView::new(store, Some(seed));
            let flow = OtpLogin::new(crate::BIN, &step, &slot)
                .interactive(prompting.interactive)
                .quiet(prompting.quiet);
            match flow.start() {
                Ok(tokens) => {
                    let item = store.load()?.unwrap_or_default();
                    keep_session(store, item, tokens).map(Started::Signed)
                }
                Err(err) => Ok(Started::WaitingOnCode(err)),
            }
        }
    }
}

/// Finish the parked sign-in with its code.
pub fn resume<T: Transport, S: TokenStore>(
    transport: &T,
    store: &S,
    code: &OtpCode,
    prompting: Prompting,
) -> Result<Credentials, CliError> {
    let step = CodeStep {
        transport,
        reached: RefCell::new(None),
    };
    let slot = ParkedView::new(store, None);
    let tokens = OtpLogin::new(crate::BIN, &step, &slot)
        .interactive(prompting.interactive)
        .quiet(prompting.quiet)
        .resume(code)?;
    let item = store.load()?.unwrap_or_default();
    keep_session(store, item, tokens)
}

fn keep_session<S: TokenStore>(
    store: &S,
    mut item: Credentials,
    tokens: Tokens,
) -> Result<Credentials, CliError> {
    item.apply_tokens(tokens, unix_now());
    item.pending = None;
    store.save(&item)?;
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::testing::{Memory, Scripted};
    use pk_cli_core::transport::HttpResponse;

    const LOGIN_PAGE: &str = include_str!("../tests/fixtures/login_page.html");
    const CODE_PAGE: &str = include_str!("../tests/fixtures/code_step.html");
    const TOKEN: &str = include_str!("../tests/fixtures/token.json");
    const QUIET: Prompting = Prompting {
        interactive: false,
        quiet: true,
    };

    fn redirect(location: &str) -> HttpResponse {
        HttpResponse::new(302, "").header("location", location)
    }

    fn to_the_password() -> Vec<HttpResponse> {
        vec![
            redirect("/Account/Login?ReturnUrl=x"),
            HttpResponse::new(200, LOGIN_PAGE),
        ]
    }

    fn start_with(script: Vec<HttpResponse>, store: &Memory) -> Result<Started, CliError> {
        let scripted = Scripted::new(script);
        start(
            &scripted,
            store,
            "dummy@example.invalid",
            "dummy-pass",
            QUIET,
        )
    }

    fn parked() -> Memory {
        let mut script = to_the_password();
        script.push(HttpResponse::new(200, CODE_PAGE));
        let store = Memory::default();
        let Started::WaitingOnCode(err) = start_with(script, &store).unwrap() else {
            panic!("expected the code step");
        };
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("--code"), "{err}");
        store
    }

    #[test]
    fn signing_in_without_a_code_keeps_the_session_and_parks_nothing() {
        let mut script = to_the_password();
        script.extend([
            redirect("com.myqops://android?code=dummycode"),
            HttpResponse::new(200, TOKEN),
        ]);
        let store = Memory::default();
        start_with(script, &store).unwrap();
        let item = store.load().unwrap().unwrap();
        assert_eq!(item.access_token.as_deref(), Some("dummy-access-token"));
        assert_eq!(item.password.as_deref(), Some("dummy-pass"));
        assert!(!item.password_blocked);
        assert!(item.pending.is_none());
    }

    #[test]
    fn a_refused_password_stores_nothing() {
        let mut script = to_the_password();
        script.push(HttpResponse::new(200, LOGIN_PAGE));
        let store = Memory::default();
        let err = start_with(script, &store).unwrap_err();
        assert!(err.to_string().contains("lock the account"), "{err}");
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn a_refused_password_is_not_mistaken_for_an_earlier_waiting_sign_in() {
        let store = parked();
        let mut script = to_the_password();
        script.push(HttpResponse::new(200, LOGIN_PAGE));
        assert!(start_with(script, &store).is_err());
    }

    #[test]
    fn a_refused_token_exchange_after_the_password_is_an_error() {
        let mut script = to_the_password();
        script.extend([
            redirect("com.myqops://android?code=dummycode"),
            HttpResponse::new(400, r#"{"error":"invalid_grant"}"#),
        ]);
        let store = Memory::default();
        let err = start_with(script, &store).unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn the_code_step_parks_the_login_and_blocks_the_password() {
        let item = parked().load().unwrap().unwrap();
        assert_eq!(item.email, "dummy@example.invalid");
        assert!(item.password_blocked);
        assert!(item.access_token.is_none());
        let waiting = item.pending.expect("parked");
        assert_eq!(waiting.sent_to_hint.as_deref(), Some("d***@example.com"));
    }

    #[test]
    fn the_code_finishes_the_parked_sign_in_and_keeps_the_login() {
        let store = parked();
        let scripted = Scripted::new(vec![
            redirect("com.myqops://android?code=dummycode"),
            HttpResponse::new(200, TOKEN),
        ]);
        let code = OtpCode::parse("123456").unwrap();
        resume(&scripted, &store, &code, QUIET).unwrap();
        let item = store.load().unwrap().unwrap();
        assert_eq!(item.access_token.as_deref(), Some("dummy-access-token"));
        assert_eq!(item.password.as_deref(), Some("dummy-pass"));
        assert!(item.password_blocked, "this login still needs a code");
        assert!(item.pending.is_none());
    }

    #[test]
    fn a_refused_code_keeps_the_sign_in_waiting() {
        let store = parked();
        let scripted = Scripted::new(vec![HttpResponse::new(200, CODE_PAGE)]);
        let code = OtpCode::parse("123456").unwrap();
        let err = resume(&scripted, &store, &code, QUIET).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        let item = store.load().unwrap().unwrap();
        assert_eq!(item.pending.expect("still parked").rejected, 1);
    }

    #[test]
    fn a_code_with_nothing_waiting_sends_nothing() {
        let scripted = Scripted::new(vec![]);
        let code = OtpCode::parse("123456").unwrap();
        let err = resume(&scripted, &Memory::default(), &code, QUIET).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(scripted.sent().is_empty());
    }

    #[test]
    fn only_a_live_session_stands_in_the_way_of_a_new_sign_in() {
        let waiting = Credentials {
            password: Some("dummy-pass".into()),
            ..Credentials::default()
        };
        let session = Credentials {
            refresh_token: Some("dummy-refresh".into()),
            ..Credentials::default()
        };
        assert!(check_overwrite(None, false, false).is_ok());
        assert!(check_overwrite(Some(&waiting), false, false).is_ok());
        assert_eq!(
            check_overwrite(Some(&session), false, false)
                .unwrap_err()
                .exit_code(),
            2
        );
        assert!(check_overwrite(Some(&session), false, true).is_ok());
        assert!(check_overwrite(Some(&waiting), true, false).is_err());
    }
}
