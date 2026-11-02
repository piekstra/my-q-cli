//! The myQ cloud client: tokens, accounts, devices and door commands.

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pk_cli_core::transport::{percent_encode, HttpRequest, HttpResponse, Transport};
use pk_cli_core::{resolve, CliError};
use serde_json::Value;

use crate::auth::{self, Begun, Pkce, Tokens};
use crate::credential::{Credentials, TokenStore};
use crate::door::{self, Account, Device, Door, DoorChange, DoorCommand};

pub const ACCOUNTS: &str = "https://accounts.myq-cloud.com";
pub const DEVICES: &str = "https://devices.myq-cloud.com";
pub const GDO: &str = "https://account-devices-gdo.myq-cloud.com";
pub const API_VERSION: &str = "v6.0";

/// Renew this long before the token runs out, so no request races its expiry.
const EXPIRY_MARGIN_SECS: u64 = 60;
const DEFAULT_LIFETIME_SECS: u64 = 1800;
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Refresh tokens rotate, so two parallel MCP calls must not spend the same one.
static RENEWAL: Mutex<()> = Mutex::new(());

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How long a door command waits for the read-back to show the target state.
#[derive(Debug, Clone, Copy)]
pub struct Wait {
    pub timeout: Duration,
    pub interval: Duration,
}

pub struct Client<T, S> {
    transport: T,
    store: S,
    cached: RefCell<Option<String>>,
    password_sign_in: bool,
}

impl<T: Transport, S: TokenStore> Client<T, S> {
    pub fn new(transport: T, store: S) -> Self {
        Self {
            transport,
            store,
            cached: RefCell::new(None),
            password_sign_in: true,
        }
    }

    /// Renew only with the refresh token. For MCP tools: posting the password can
    /// make myQ send a one-time code, which a tool call has no way to answer.
    pub fn without_password_sign_in(mut self) -> Self {
        self.password_sign_in = false;
        self
    }

    fn apply(&self, mut credentials: Credentials, tokens: Tokens) -> Result<String, CliError> {
        let access = tokens.access_token;
        credentials.access_token = Some(access.clone());
        if tokens.refresh_token.is_some() {
            credentials.refresh_token = tokens.refresh_token;
        }
        let lifetime = tokens.expires_in.unwrap_or(DEFAULT_LIFETIME_SECS);
        credentials.expires_at = Some(unix_now() + lifetime);
        self.store.save(&credentials)?;
        Ok(access)
    }

    /// Refresh, then fall back to the stored password when the refresh token is finished.
    async fn renew(&self, credentials: Credentials) -> Result<String, CliError> {
        let password = credentials
            .password
            .clone()
            .filter(|_| self.password_sign_in && !credentials.code_required);
        if let Some(token) = credentials.refresh_token.clone() {
            match auth::refresh(&self.transport, &token).await {
                Ok(tokens) => return self.apply(credentials, tokens),
                Err(CliError::Auth(_)) if password.is_some() => {}
                Err(CliError::Auth(_)) => return Err(sign_in_needed()),
                Err(other) => return Err(other),
            }
        }
        let Some(password) = password else {
            return Err(sign_in_needed());
        };
        let pkce = Pkce::generate()?;
        match auth::begin(&self.transport, &credentials.email, &password, &pkce).await? {
            Begun::Signed(tokens) => self.apply(credentials, tokens),
            // Only a person can answer this. Remembering it stops a later read or
            // MCP call from making myQ send another code.
            Begun::Code(_) => {
                let mut credentials = credentials;
                credentials.code_required = true;
                self.store.save(&credentials)?;
                Err(sign_in_needed())
            }
        }
    }

    #[allow(clippy::await_holding_lock)] // the transport is blocking natively, and this serializes renewal on purpose
    async fn bearer(&self, stale: Option<&str>) -> Result<String, CliError> {
        if stale.is_none() {
            if let Some(token) = self.cached.borrow().clone() {
                return Ok(token);
            }
        }
        let _renewing = RENEWAL.lock().unwrap_or_else(|e| e.into_inner());
        let credentials = self.store.load()?.ok_or_else(sign_in_needed)?;
        let still_good = credentials
            .expires_at
            .is_some_and(|at| at > unix_now() + EXPIRY_MARGIN_SECS);
        let token = match credentials.access_token.clone() {
            // A parallel call may already have renewed what this one saw rejected.
            Some(token) if still_good && stale != Some(token.as_str()) => token,
            _ => self.renew(credentials).await?,
        };
        *self.cached.borrow_mut() = Some(token.clone());
        Ok(token)
    }

    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, CliError> {
        let token = self.bearer(None).await?;
        let response = self.transport.send(signed(request.clone(), &token)).await?;
        if response.status != 401 {
            return Ok(response);
        }
        let token = self.bearer(Some(&token)).await?;
        self.transport.send(signed(request, &token)).await
    }

    async fn get_json(&self, url: String, what: &str) -> Result<Value, CliError> {
        let response = self.send(HttpRequest::get(url)).await?;
        check(&response, what)?;
        parse_json(&response, what)
    }

    pub async fn accounts(&self) -> Result<Vec<Account>, CliError> {
        let url = format!("{ACCOUNTS}/api/{API_VERSION}/accounts");
        Ok(door::accounts_from(&self.get_json(url, "accounts").await?))
    }

    /// The account to act on: the one asked for, or the login's only one.
    pub async fn account_id(&self, preferred: Option<&str>) -> Result<String, CliError> {
        let accounts = self.accounts().await?;
        if let Some(query) = preferred {
            let found = resolve::pick(
                &accounts,
                query,
                |a| vec![a.id.clone()],
                |a| &a.name,
                "account",
            )?;
            return Ok(found.id.clone());
        }
        match accounts.as_slice() {
            [] => Err(CliError::NotFound("this login has no myQ account".into())),
            [only] => Ok(only.id.clone()),
            many => Err(CliError::Usage(format!(
                "this login has {} accounts; pick one with --account or `myq config set account <id>`: {}",
                many.len(),
                many.iter()
                    .map(|a| format!("{} ({})", a.name, a.id))
                    .collect::<Vec<_>>()
                    .join("; ")
            ))),
        }
    }

    async fn devices_json(&self, account: &str) -> Result<Value, CliError> {
        let url = format!(
            "{DEVICES}/api/{API_VERSION}/accounts/{}/devices",
            percent_encode(account)
        );
        self.get_json(url, "devices").await
    }

    pub async fn devices(&self, account: &str) -> Result<Vec<Device>, CliError> {
        Ok(door::devices_from(&self.devices_json(account).await?))
    }

    pub async fn doors(&self, account: &str) -> Result<Vec<Door>, CliError> {
        Ok(door::doors_from(&self.devices_json(account).await?))
    }

    async fn door_command(
        &self,
        account: &str,
        serial: &str,
        command: DoorCommand,
    ) -> Result<(), CliError> {
        let url = format!(
            "{GDO}/api/{API_VERSION}/accounts/{}/door_openers/{}/{}",
            percent_encode(account),
            percent_encode(serial),
            command.path()
        );
        // An empty body, not none: a PUT without Content-Length is refused by some servers.
        let response = self.send(HttpRequest::new("PUT", url).body("")).await?;
        check(&response, "the door command")
    }

    /// Move a door and report what a read-back shows. `pause` waits between
    /// reads; `wait.timeout` of zero sends the command and reads once.
    pub async fn set_door(
        &self,
        account: &str,
        door_ref: &str,
        command: DoorCommand,
        wait: Wait,
        pause: &dyn Fn(Duration),
    ) -> Result<DoorChange, CliError> {
        let doors = self.doors(account).await?;
        let door = door::find(&doors, door_ref)?.clone();
        if door.state == command.target() {
            return Ok(DoorChange {
                door,
                changed: false,
                confirmed: true,
            });
        }
        if door.online == Some(false) {
            return Err(CliError::Upstream(format!(
                "`{}` is offline, so myQ cannot move it",
                door.name
            )));
        }
        self.door_command(account, &door.serial, command).await?;

        let polls = wait.timeout.as_secs() / wait.interval.as_secs().max(1);
        let read = |door: Door| async move {
            let doors = self.doors(account).await?;
            let read_back = doors.into_iter().find(|d| d.serial == door.serial);
            read_back.ok_or_else(|| {
                CliError::Upstream(format!("`{}` vanished from myQ's device list", door.name))
            })
        };
        if polls == 0 {
            return Ok(DoorChange {
                door: read(door).await?,
                changed: true,
                confirmed: false,
            });
        }
        let mut latest = door;
        for _ in 0..polls {
            pause(wait.interval);
            latest = read(latest).await?;
            if latest.state == command.target() {
                return Ok(DoorChange {
                    door: latest,
                    changed: true,
                    confirmed: true,
                });
            }
        }
        Err(CliError::Upstream(format!(
            "myQ accepted the command, but `{}` still reads `{}` after {} seconds. \
             Check the door before sending it again",
            latest.name,
            latest.state,
            wait.timeout.as_secs()
        )))
    }

    /// The `api` passthrough. Only myQ's own hosts get the token.
    pub async fn raw(
        &self,
        method: &str,
        url: &str,
        body: Option<String>,
    ) -> Result<Value, CliError> {
        ensure_myq_host(url)?;
        let mut request = HttpRequest::new(method, url);
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body);
        } else if method != "GET" {
            request = request.body("");
        }
        let response = self.send(request).await?;
        check(&response, "the request")?;
        if response.body.trim().is_empty() {
            return Ok(serde_json::json!({ "status": response.status }));
        }
        parse_json(&response, "the request")
    }
}

fn sign_in_needed() -> CliError {
    CliError::Auth("not signed in to myQ; run `myq auth login`".into())
}

fn signed(request: HttpRequest, token: &str) -> HttpRequest {
    request
        .user_agent(crate::BIN, crate::VERSION)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {token}"))
}

/// Refuses a URL whose host is not `myq-cloud.com` or under it, label by label.
pub fn ensure_myq_host(url: &str) -> Result<(), CliError> {
    let host = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .unwrap_or("");
    if host == "myq-cloud.com" || host.ends_with(".myq-cloud.com") {
        return Ok(());
    }
    Err(CliError::Usage(
        "`api` only calls https URLs on myq-cloud.com, which is where the token belongs".into(),
    ))
}

/// A short reason from an error body, without echoing the whole response.
fn reason(response: &HttpResponse) -> String {
    let parsed: Option<Value> = serde_json::from_str(&response.body).ok();
    let found = parsed.as_ref().and_then(|v| {
        [
            "description",
            "message",
            "error_description",
            "error",
            "title",
        ]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
    });
    match found {
        Some(text) => format!(": {}", text.chars().take(200).collect::<String>()),
        None => String::new(),
    }
}

fn check(response: &HttpResponse, what: &str) -> Result<(), CliError> {
    let detail = reason(response);
    let status = response.status;
    if response.header_value("cf-mitigated").is_some() {
        return Err(CliError::Upstream(format!(
            "Cloudflare refused {what} (HTTP {status})"
        )));
    }
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(CliError::Auth(format!(
            "myQ refused {what} (HTTP {status}{detail}); run `myq auth login` if this persists"
        ))),
        404 => Err(CliError::NotFound(format!(
            "myQ has no such resource for {what} (HTTP 404{detail})"
        ))),
        _ => Err(CliError::Upstream(format!(
            "myQ answered {what} with HTTP {status}{detail}"
        ))),
    }
}

fn parse_json(response: &HttpResponse, what: &str) -> Result<Value, CliError> {
    serde_json::from_str(&response.body).map_err(|_| {
        CliError::Upstream(format!(
            "myQ answered {what} with HTTP {} and a body that is not JSON",
            response.status
        ))
    })
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::collections::VecDeque;

    /// Answers each send with the next scripted response and records the requests.
    pub struct Scripted {
        responses: RefCell<VecDeque<HttpResponse>>,
        sent: RefCell<Vec<HttpRequest>>,
    }

    impl Scripted {
        pub fn new(responses: Vec<HttpResponse>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                sent: RefCell::new(Vec::new()),
            }
        }

        pub fn sent(&self) -> Vec<HttpRequest> {
            self.sent.borrow().clone()
        }
    }

    impl Transport for Scripted {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse, CliError> {
            self.sent.borrow_mut().push(request);
            self.responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| CliError::Other("no scripted response left".into()))
        }
    }

    pub fn header_of(request: &HttpRequest, name: &str) -> Option<String> {
        request.header_value(name).map(str::to_string)
    }

    #[derive(Default)]
    pub struct Memory(pub RefCell<Option<Credentials>>);

    impl TokenStore for Memory {
        fn load(&self) -> Result<Option<Credentials>, CliError> {
            Ok(self.0.borrow().clone())
        }
        fn save(&self, credentials: &Credentials) -> Result<(), CliError> {
            *self.0.borrow_mut() = Some(credentials.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use pk_cli_core::block_on;

    const TOKEN: &str = include_str!("../tests/fixtures/token.json");
    const DEVICES_JSON: &str = include_str!("../tests/fixtures/devices.json");
    const ACCOUNTS_JSON: &str = include_str!("../tests/fixtures/accounts.json");
    const ACCOUNT: &str = "00000000-0000-0000-0000-000000000001";

    fn signed_in() -> Memory {
        Memory(RefCell::new(Some(Credentials {
            email: "dummy@example.invalid".into(),
            password: Some("dummy-pass".into()),
            access_token: Some("dummy-live".into()),
            refresh_token: Some("dummy-refresh-old".into()),
            expires_at: Some(unix_now() + 3600),
            ..Credentials::default()
        })))
    }

    fn expired() -> Memory {
        let store = signed_in();
        store.0.borrow_mut().as_mut().unwrap().expires_at = Some(unix_now() - 1);
        store
    }

    const FAST: Wait = Wait {
        timeout: Duration::from_secs(9),
        interval: Duration::from_secs(3),
    };

    /// `devices.json` with its first door's `door_state` replaced.
    fn devices_with_left(state: &str) -> HttpResponse {
        HttpResponse::new(
            200,
            DEVICES_JSON.replacen("\"closed\"", &format!("\"{state}\""), 1),
        )
    }

    #[test]
    fn a_live_token_is_used_as_is() {
        let scripted = Scripted::new(vec![HttpResponse::new(200, DEVICES_JSON)]);
        let client = Client::new(&scripted, signed_in());
        let doors = block_on(client.doors(ACCOUNT)).unwrap();
        assert_eq!(doors.len(), 2);
        let sent = scripted.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].url,
            format!("https://devices.myq-cloud.com/api/v6.0/accounts/{ACCOUNT}/devices")
        );
        assert_eq!(
            header_of(&sent[0], "authorization").as_deref(),
            Some("Bearer dummy-live")
        );
    }

    #[test]
    fn an_expired_token_refreshes_and_the_rotated_tokens_are_saved() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(200, TOKEN),
            HttpResponse::new(200, DEVICES_JSON),
        ]);
        let store = expired();
        let client = Client::new(&scripted, &store);
        block_on(client.doors(ACCOUNT)).unwrap();
        let sent = scripted.sent();
        assert!(sent[0]
            .body
            .as_deref()
            .unwrap()
            .contains("refresh_token=dummy-refresh-old"));
        assert_eq!(
            header_of(&sent[1], "authorization").as_deref(),
            Some("Bearer dummy-access-token")
        );
        let saved = store.0.borrow().clone().unwrap();
        assert_eq!(saved.refresh_token.as_deref(), Some("dummy-refresh-token"));
        assert!(saved.expires_at.unwrap() > unix_now());
    }

    #[test]
    fn a_401_renews_once_and_retries() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(401, ""),
            HttpResponse::new(200, TOKEN),
            HttpResponse::new(200, DEVICES_JSON),
        ]);
        let client = Client::new(&scripted, signed_in());
        block_on(client.doors(ACCOUNT)).unwrap();
        assert_eq!(scripted.sent().len(), 3);
    }

    #[test]
    fn a_second_401_after_renewing_is_an_auth_error_and_stops() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(401, ""),
            HttpResponse::new(200, TOKEN),
            HttpResponse::new(401, ""),
        ]);
        let client = Client::new(&scripted, signed_in());
        assert_eq!(block_on(client.doors(ACCOUNT)).unwrap_err().exit_code(), 3);
        assert_eq!(scripted.sent().len(), 3);
    }

    #[test]
    fn a_dead_refresh_token_falls_back_to_the_password() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(400, r#"{"error":"invalid_grant"}"#),
            HttpResponse::new(302, "").header("location", "com.myqops://android?code=dummycode"),
            HttpResponse::new(200, TOKEN),
            HttpResponse::new(200, DEVICES_JSON),
        ]);
        let store = expired();
        let client = Client::new(&scripted, &store);
        block_on(client.doors(ACCOUNT)).unwrap();
        assert_eq!(
            store.0.borrow().clone().unwrap().access_token.as_deref(),
            Some("dummy-access-token")
        );
    }

    #[test]
    fn a_dead_session_on_a_code_login_never_sends_the_password() {
        let scripted = Scripted::new(vec![HttpResponse::new(400, r#"{"error":"invalid_grant"}"#)]);
        let store = expired();
        store.0.borrow_mut().as_mut().unwrap().code_required = true;
        let client = Client::new(&scripted, &store);
        let err = block_on(client.doors(ACCOUNT)).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("myq auth login"), "{err}");
        assert_eq!(scripted.sent().len(), 1, "only the refresh was tried");
    }

    #[test]
    fn a_tool_client_renews_with_the_refresh_token_and_never_the_password() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(200, TOKEN),
            HttpResponse::new(200, DEVICES_JSON),
        ]);
        let client = Client::new(&scripted, expired()).without_password_sign_in();
        block_on(client.doors(ACCOUNT)).unwrap();

        let scripted = Scripted::new(vec![HttpResponse::new(400, r#"{"error":"invalid_grant"}"#)]);
        let client = Client::new(&scripted, expired()).without_password_sign_in();
        let err = block_on(client.doors(ACCOUNT)).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert_eq!(scripted.sent().len(), 1, "the password was not posted");
    }

    #[test]
    fn a_login_that_starts_asking_for_a_code_is_remembered() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(400, r#"{"error":"invalid_grant"}"#),
            HttpResponse::new(200, include_str!("../tests/fixtures/login_page.html")),
            HttpResponse::new(200, include_str!("../tests/fixtures/code_step.html")),
        ]);
        let store = expired();
        let client = Client::new(&scripted, &store);
        let err = block_on(client.doors(ACCOUNT)).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(store.0.borrow().as_ref().unwrap().code_required);

        let scripted = Scripted::new(vec![HttpResponse::new(400, r#"{"error":"invalid_grant"}"#)]);
        let client = Client::new(&scripted, &store);
        block_on(client.doors(ACCOUNT)).unwrap_err();
        assert_eq!(scripted.sent().len(), 1, "the password is not tried again");
    }

    #[test]
    fn no_stored_credential_is_exit_3_and_sends_nothing() {
        let scripted = Scripted::new(vec![]);
        let client = Client::new(&scripted, Memory::default());
        let err = block_on(client.accounts()).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("myq auth login"));
        assert!(scripted.sent().is_empty());
    }

    #[test]
    fn the_only_account_is_chosen_and_several_need_a_choice() {
        let scripted = Scripted::new(vec![HttpResponse::new(200, ACCOUNTS_JSON)]);
        let client = Client::new(&scripted, signed_in());
        assert_eq!(block_on(client.account_id(None)).unwrap(), ACCOUNT);
        assert_eq!(
            scripted.sent()[0].url,
            "https://accounts.myq-cloud.com/api/v6.0/accounts"
        );

        let two = r#"{"accounts":[{"id":"a1","name":"One"},{"id":"a2","name":"Two"}]}"#;
        let scripted = Scripted::new(vec![
            HttpResponse::new(200, two),
            HttpResponse::new(200, two),
        ]);
        let client = Client::new(&scripted, signed_in());
        assert_eq!(
            block_on(client.account_id(None)).unwrap_err().exit_code(),
            2
        );
        assert_eq!(block_on(client.account_id(Some("two"))).unwrap(), "a2");
    }

    #[test]
    fn opening_sends_a_put_and_succeeds_only_when_the_read_back_says_open() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(200, DEVICES_JSON),
            HttpResponse::new(204, ""),
            devices_with_left("opening"),
            devices_with_left("open"),
        ]);
        let client = Client::new(&scripted, signed_in());
        let pauses = RefCell::new(0);
        let change = block_on(
            client.set_door(ACCOUNT, "left", DoorCommand::Open, FAST, &|_| {
                *pauses.borrow_mut() += 1;
            }),
        )
        .unwrap();
        assert!(change.changed && change.confirmed);
        assert_eq!(change.door.state, "open");
        assert_eq!(*pauses.borrow(), 2);
        let put = &scripted.sent()[1];
        assert_eq!(put.method, "PUT");
        assert_eq!(
            put.url,
            format!("https://account-devices-gdo.myq-cloud.com/api/v6.0/accounts/{ACCOUNT}/door_openers/DUMMYSERIAL01/open")
        );
        assert_eq!(put.body.as_deref(), Some(""));
    }

    #[test]
    fn a_200_that_never_moves_the_door_is_a_failure_not_a_success() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(200, DEVICES_JSON),
            HttpResponse::new(200, ""),
            HttpResponse::new(200, DEVICES_JSON),
            HttpResponse::new(200, DEVICES_JSON),
            HttpResponse::new(200, DEVICES_JSON),
        ]);
        let client = Client::new(&scripted, signed_in());
        let err = block_on(client.set_door(ACCOUNT, "left", DoorCommand::Open, FAST, &|_| {}))
            .unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("still reads `closed`"), "{err}");
    }

    #[test]
    fn a_door_already_in_the_target_state_gets_no_command() {
        let scripted = Scripted::new(vec![HttpResponse::new(200, DEVICES_JSON)]);
        let client = Client::new(&scripted, signed_in());
        let change =
            block_on(client.set_door(ACCOUNT, "left", DoorCommand::Close, FAST, &|_| {})).unwrap();
        assert!(!change.changed && change.confirmed);
        assert_eq!(scripted.sent().len(), 1);
    }

    #[test]
    fn an_offline_door_gets_no_command() {
        let scripted = Scripted::new(vec![HttpResponse::new(200, DEVICES_JSON)]);
        let client = Client::new(&scripted, signed_in());
        let err = block_on(client.set_door(ACCOUNT, "right", DoorCommand::Close, FAST, &|_| {}))
            .unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("offline"));
        assert_eq!(scripted.sent().len(), 1);
    }

    #[test]
    fn no_wait_reads_once_and_does_not_claim_confirmation() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(200, DEVICES_JSON),
            HttpResponse::new(204, ""),
            devices_with_left("opening"),
        ]);
        let client = Client::new(&scripted, signed_in());
        let no_wait = Wait {
            timeout: Duration::ZERO,
            interval: POLL_INTERVAL,
        };
        let change =
            block_on(client.set_door(ACCOUNT, "left", DoorCommand::Open, no_wait, &|_| {}))
                .unwrap();
        assert!(change.changed && !change.confirmed);
        assert_eq!(change.door.state, "opening");
    }

    #[test]
    fn the_token_only_goes_to_myq_hosts() {
        for ok in [
            "https://devices.myq-cloud.com/api",
            "https://myq-cloud.com",
            "https://a.b.myq-cloud.com?x=1",
        ] {
            assert!(ensure_myq_host(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://devices.myq-cloud.com/api",
            "https://myq-cloud.com.evil.example/",
            "https://notmyq-cloud.com/",
            "https://evil.example/?x=.myq-cloud.com",
            "https://evil.example#.myq-cloud.com",
            "/api/x",
        ] {
            assert_eq!(ensure_myq_host(bad).unwrap_err().exit_code(), 2, "{bad}");
        }
    }

    #[test]
    fn an_error_reason_comes_from_the_body_without_echoing_it_whole() {
        let scripted = Scripted::new(vec![HttpResponse::new(
            500,
            r#"{"description":"dummy reason","secretish":"dummy-other"}"#,
        )]);
        let client = Client::new(&scripted, signed_in());
        let text = block_on(client.accounts()).unwrap_err().to_string();
        assert!(text.contains("dummy reason"));
        assert!(!text.contains("dummy-other"));
    }

    #[test]
    fn a_cloudflare_block_is_upstream_rather_than_a_bad_login() {
        let scripted = Scripted::new(vec![
            HttpResponse::new(403, "").header("cf-mitigated", "challenge")
        ]);
        let client = Client::new(&scripted, signed_in());
        assert_eq!(block_on(client.accounts()).unwrap_err().exit_code(), 5);
    }
}
