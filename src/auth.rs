//! Signing in to myQ's identity server, as the Android app does: an OAuth
//! authorization-code flow with PKCE, for a public client (no client secret),
//! where the sign-in page is an ordinary server-rendered form.
//!
//! Redirects and cookies are handled here because the transport does neither,
//! and because the last redirect goes to the app's custom scheme, which is the
//! only place the authorization code ever appears.

use std::cell::{Cell, RefCell};
use std::fmt;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use pk_cli_auth::otp::{CodeSent, OtpCode, OtpLogin, OtpTransport};
use pk_cli_core::transport::{percent_encode, HttpRequest, HttpResponse, Transport};
use pk_cli_core::{block_on, CliError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const IDS: &str = "https://partner-identity.myq-cloud.com";
pub const CLIENT_ID: &str = "ANDROID_CGI_MYQ";
pub const SCOPE: &str = "MyQ_Residential offline_access";
pub const REDIRECT_URI: &str = "com.myqops://android";
/// The app's `"%s %s brand:%s"` with an empty create-user part: hence two spaces.
const ACR_VALUES: &str = "unified_flow:v1  brand:myq";
const MAX_HOPS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn from_random(random: &[u8; 32]) -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(random);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }

    pub fn generate() -> Result<Self, CliError> {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random)
            .map_err(|e| CliError::Other(format!("no system randomness: {e}")))?;
        Ok(Self::from_random(&random))
    }
}

#[derive(Clone, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("refresh_token", &self.refresh_token.is_some())
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// One sign-in's cookies. Every hop stays on the identity host, so one jar with
/// no domain logic is enough, and `next_url` refuses to leave that host.
#[derive(Default, Clone, Serialize, Deserialize)]
struct Jar(Vec<(String, String)>);

impl Jar {
    fn absorb(&mut self, response: &HttpResponse) {
        for (name, value) in &response.headers {
            if name != "set-cookie" {
                continue;
            }
            let pair = value.split(';').next().unwrap_or("");
            if let Some((k, v)) = pair.split_once('=') {
                let (k, v) = (k.trim().to_string(), v.trim().to_string());
                self.0.retain(|(existing, _)| *existing != k);
                self.0.push((k, v));
            }
        }
    }

    fn header(&self) -> String {
        self.0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn apply(&self, request: HttpRequest) -> HttpRequest {
        if self.0.is_empty() {
            request
        } else {
            request.header("Cookie", self.header())
        }
    }
}

fn base(request: HttpRequest) -> HttpRequest {
    request.user_agent(crate::BIN, crate::VERSION)
}

pub fn authorize_url(challenge: &str) -> String {
    HttpRequest::get(format!("{IDS}/connect/authorize"))
        .query(&[
            ("client_id", CLIENT_ID.to_string()),
            ("scope", SCOPE.to_string()),
            ("response_type", "code".to_string()),
            ("redirect_uri", REDIRECT_URI.to_string()),
            ("code_challenge", challenge.to_string()),
            ("code_challenge_method", "S256".to_string()),
            ("ui_locales", "en-US".to_string()),
            ("acr_values", ACR_VALUES.to_string()),
            ("prompt", "login".to_string()),
        ])
        .url
}

fn next_url(location: &str) -> Result<String, CliError> {
    if location.starts_with('/') {
        return Ok(format!("{IDS}{location}"));
    }
    if location == IDS || location.starts_with(&format!("{IDS}/")) {
        return Ok(location.to_string());
    }
    let host = location
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(location);
    Err(CliError::Upstream(format!(
        "myQ sign-in redirected to an unexpected host ({host})"
    )))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The `code` parameter of the redirect to the app's custom scheme.
pub fn code_from_redirect(location: &str) -> Option<String> {
    let query = location.split_once('?')?.1;
    query
        .split(['&', '#'])
        .find_map(|pair| pair.strip_prefix("code="))
        .map(percent_decode)
        .filter(|code| !code.is_empty())
}

enum Landing {
    Code(String),
    /// The page and the URL it came from, which a form with a relative or empty
    /// action posts back to.
    Page(HttpResponse, String),
}

fn refused(response: &HttpResponse) -> Option<CliError> {
    let challenged = response.header_value("cf-mitigated").is_some()
        || (response.status == 403 && response.body.contains("Just a moment"));
    challenged.then(|| {
        CliError::Upstream(format!(
            "Cloudflare refused the request to myQ's sign-in (HTTP {})",
            response.status
        ))
    })
}

/// Follow redirects until a page or the authorization code.
async fn land<T: Transport>(
    transport: &T,
    jar: &mut Jar,
    mut response: HttpResponse,
    mut url: String,
) -> Result<Landing, CliError> {
    for _ in 0..MAX_HOPS {
        jar.absorb(&response);
        if let Some(err) = refused(&response) {
            return Err(err);
        }
        if !(300..400).contains(&response.status) {
            if response.is_success() {
                return Ok(Landing::Page(response, url));
            }
            return Err(CliError::Upstream(format!(
                "myQ sign-in answered HTTP {}",
                response.status
            )));
        }
        let location = response
            .header_value("location")
            .ok_or_else(|| CliError::Upstream("myQ sign-in redirected with no Location".into()))?
            .to_string();
        if location.starts_with(REDIRECT_URI) {
            return code_from_redirect(&location)
                .map(Landing::Code)
                .ok_or_else(|| {
                    CliError::Auth(
                        "myQ's sign-in finished without an authorization code; \
                     run `myq auth login` again"
                            .into(),
                    )
                });
        }
        url = next_url(&location)?;
        response = transport
            .send(jar.apply(base(HttpRequest::get(url.clone()))))
            .await?;
    }
    Err(CliError::Upstream(format!(
        "myQ sign-in redirected more than {MAX_HOPS} times"
    )))
}

/// The sign-in form: its target, and the hidden fields to send back unchanged.
#[derive(Debug, PartialEq, Eq)]
pub struct LoginForm {
    pub action: String,
    pub hidden: Vec<(String, String)>,
}

/// `None` when the page has no form with a password field.
pub fn parse_login_form(html: &str) -> Option<LoginForm> {
    for body in forms(html) {
        let tags = input_tags(body);
        let has_password = tags
            .iter()
            .any(|t| pk_cli_scrape::attr(t, "type").as_deref() == Some("password"));
        if !has_password {
            continue;
        }
        let form_tag = &body[..body.find('>')? + 1];
        let action = pk_cli_scrape::attr(form_tag, "action")?;
        let hidden = tags
            .iter()
            .filter(|t| pk_cli_scrape::attr(t, "type").as_deref() == Some("hidden"))
            .filter_map(|t| {
                let name = pk_cli_scrape::attr(t, "name")?;
                let value = pk_cli_scrape::attr(t, "value").unwrap_or_default();
                Some((name, pk_cli_scrape::decode_entities(&value)))
            })
            .collect();
        return Some(LoginForm {
            action: pk_cli_scrape::decode_entities(&action),
            hidden,
        });
    }
    None
}

/// Each `<form>` on the page, from its opening tag up to its closing tag.
fn forms(html: &str) -> Vec<&str> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("<form") {
        let start = from + rel;
        let Some(len) = lower[start..].find("</form") else {
            break;
        };
        found.push(&html[start..start + len]);
        from = start + len + 1;
    }
    found
}

/// The one-time-code step: the form holding the code field, and what a browser
/// would send along with the code.
#[derive(Debug, PartialEq, Eq)]
pub struct CodeStep {
    /// The form's action as written; empty when it has none.
    pub action: String,
    pub field: String,
    pub fields: Vec<(String, String)>,
    pub channel: Option<String>,
    pub sent_to_hint: Option<String>,
}

const CODE_FIELD: &str = "Otp";

/// `None` when no form on the page has a code field.
pub fn parse_code_step(html: &str) -> Option<CodeStep> {
    for body in forms(html) {
        let tags = input_tags(body);
        let name_of = |t: &String| pk_cli_scrape::attr(t, "name");
        let Some(field) = tags
            .iter()
            .filter_map(name_of)
            .find(|n| n.eq_ignore_ascii_case(CODE_FIELD))
        else {
            continue;
        };
        let form_tag = &body[..body.find('>')? + 1];
        let fields: Vec<(String, String)> = tags
            .iter()
            .filter(|t| submits_with_the_form(t))
            .filter_map(|t| {
                let name = name_of(t)?;
                let value = pk_cli_scrape::attr(t, "value").unwrap_or_default();
                Some((name, pk_cli_scrape::decode_entities(&value)))
            })
            .filter(|(name, _)| *name != field)
            .collect();
        let value_of = |wanted: &str| {
            fields
                .iter()
                .find(|(name, value)| name == wanted && !value.is_empty())
                .map(|(_, value)| value.clone())
        };
        let channel = value_of("SelectedMfaMethod");
        let to_phone = channel.as_deref().is_some_and(|c| {
            let c = c.to_ascii_lowercase();
            c.contains("sms") || c.contains("phone") || c.contains("text")
        });
        let sent_to_hint = if to_phone {
            value_of("MaskedPhoneNumber")
        } else {
            value_of("MaskedEmail")
        };
        return Some(CodeStep {
            action: pk_cli_scrape::attr(form_tag, "action")
                .map(|a| pk_cli_scrape::decode_entities(&a))
                .unwrap_or_default(),
            field,
            fields,
            channel,
            sent_to_hint,
        });
    }
    None
}

/// What a browser submits: named inputs, minus buttons and unticked boxes.
fn submits_with_the_form(tag: &str) -> bool {
    let kind = pk_cli_scrape::attr(tag, "type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match kind.as_str() {
        "submit" | "button" | "image" | "reset" | "file" => false,
        "radio" | "checkbox" => tag.to_ascii_lowercase().contains(" checked"),
        _ => true,
    }
}

/// Where a form on the page at `page` posts to.
fn form_target(action: &str, page: &str) -> Result<String, CliError> {
    let path = page.split('?').next().unwrap_or(page);
    if action.is_empty() {
        return Ok(page.to_string());
    }
    if action.starts_with('?') {
        return Ok(format!("{path}{action}"));
    }
    if action.starts_with('/') || action.contains("://") {
        return next_url(action);
    }
    let dir = path.rsplit_once('/').map_or(path, |(dir, _)| dir);
    next_url(&format!("{dir}/{action}"))
}

fn input_tags(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut tags = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("<input") {
        let start = from + rel;
        let Some(len) = lower[start..].find('>') else {
            break;
        };
        tags.push(html[start..=start + len].to_string());
        from = start + len + 1;
    }
    tags
}

fn form_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Why the identity server stayed on a page after the password was posted. The
/// page's input names help diagnose a new step; their values never appear.
fn stopped_at(page: &HttpResponse) -> CliError {
    if parse_login_form(&page.body).is_some() {
        return CliError::Auth(
            "myQ rejected the email or password. Repeated failures can lock the account, \
             so check the stored password before retrying"
                .into(),
        );
    }
    let names: Vec<String> = input_tags(&page.body)
        .iter()
        .filter_map(|t| pk_cli_scrape::attr(t, "name"))
        .filter(|n| !n.starts_with("__"))
        .collect();
    let targets: Vec<String> = forms(&page.body)
        .iter()
        .filter_map(|form| {
            let tag = &form[..form.find('>')? + 1];
            let action = pk_cli_scrape::attr(tag, "action")?;
            Some(action.split('?').next().unwrap_or("").to_string())
        })
        .collect();
    let list = |items: Vec<String>| {
        if items.is_empty() {
            "none".to_string()
        } else {
            items.join(", ")
        }
    };
    CliError::Auth(format!(
        "myQ asked for a further sign-in step this CLI does not handle yet \
         (forms post to: {}; fields on the page: {})",
        list(targets),
        list(names)
    ))
}

/// A sign-in waiting on its one-time code: everything the code's request needs.
/// Kept in the keychain between `auth login` and `auth login --code`, because a
/// code only counts against the session that asked for it.
#[derive(Clone, Serialize, Deserialize)]
pub struct Pending {
    verifier: String,
    cookies: Jar,
    target: String,
    field: String,
    fields: Vec<(String, String)>,
    #[serde(skip)]
    channel: Option<String>,
    #[serde(skip)]
    sent_to_hint: Option<String>,
}

impl fmt::Debug for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Pending(***)")
    }
}

#[derive(Debug)]
pub enum Begun {
    Signed(Tokens),
    Code(Pending),
}

/// From the authorize request through the password: a token, or the code step.
pub async fn begin<T: Transport>(
    transport: &T,
    email: &str,
    password: &str,
    pkce: &Pkce,
) -> Result<Begun, CliError> {
    let mut jar = Jar::default();
    let authorize = authorize_url(&pkce.challenge);
    let first = transport
        .send(base(HttpRequest::get(authorize.clone())))
        .await?;
    let page = match land(transport, &mut jar, first, authorize).await? {
        Landing::Page(page, _) => page,
        Landing::Code(code) => {
            return exchange(transport, &code, &pkce.verifier)
                .await
                .map(Begun::Signed)
        }
    };
    let form = parse_login_form(&page.body).ok_or_else(|| {
        CliError::Upstream("myQ's sign-in page has no password form; the page has changed".into())
    })?;
    let mut fields: Vec<(&str, &str)> = form
        .hidden
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    fields.push(("Email", email));
    fields.push(("Password", password));
    let target = next_url(&form.action)?;
    let post = HttpRequest::new("POST", target.clone())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_body(&fields));
    let posted = transport.send(jar.apply(base(post))).await?;
    match land(transport, &mut jar, posted, target).await? {
        Landing::Code(code) => exchange(transport, &code, &pkce.verifier)
            .await
            .map(Begun::Signed),
        Landing::Page(page, url) => {
            let Some(step) = parse_code_step(&page.body) else {
                return Err(stopped_at(&page));
            };
            Ok(Begun::Code(Pending {
                verifier: pkce.verifier.clone(),
                cookies: jar,
                target: form_target(&step.action, &url)?,
                field: step.field,
                fields: step.fields,
                channel: step.channel,
                sent_to_hint: step.sent_to_hint,
            }))
        }
    }
}

/// Present a one-time code against the sign-in that asked for it. `Auth` means
/// myQ refused the code; the parked sign-in survives for another try.
pub async fn redeem<T: Transport>(
    transport: &T,
    pending: &Pending,
    code: &str,
) -> Result<Tokens, CliError> {
    let mut jar = pending.cookies.clone();
    let mut fields: Vec<(&str, &str)> = pending
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    fields.push((pending.field.as_str(), code));
    let post = HttpRequest::new("POST", pending.target.clone())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_body(&fields));
    let posted = transport.send(jar.apply(base(post))).await?;
    match land(transport, &mut jar, posted, pending.target.clone()).await? {
        Landing::Code(code) => exchange(transport, &code, &pending.verifier).await,
        Landing::Page(page, _) if parse_code_step(&page.body).is_some() => {
            Err(CliError::Auth("myQ refused that code".into()))
        }
        Landing::Page(page, _) if parse_login_form(&page.body).is_some() => {
            Err(CliError::Auth(format!(
                "myQ's sign-in session ended before the code was checked; \
                 run `{} auth login` for a new code",
                crate::BIN
            )))
        }
        Landing::Page(page, _) => Err(stopped_at(&page)),
    }
}

/// The provider side of `auth login` for the shared one-time-code flow.
pub struct SignIn<'a, T> {
    transport: &'a T,
    login: Option<(&'a str, &'a str)>,
    signed_without_code: RefCell<Option<Tokens>>,
    reached_code_step: Cell<bool>,
}

impl<'a, T: Transport> SignIn<'a, T> {
    /// A sign-in starting from the email and password.
    pub fn start(transport: &'a T, email: &'a str, password: &'a str) -> Self {
        Self {
            transport,
            login: Some((email, password)),
            signed_without_code: RefCell::new(None),
            reached_code_step: Cell::new(false),
        }
    }

    /// Finishing a parked sign-in, which needs only the code.
    pub fn resume(transport: &'a T) -> Self {
        Self {
            transport,
            login: None,
            signed_without_code: RefCell::new(None),
            reached_code_step: Cell::new(false),
        }
    }

    pub fn reached_code_step(&self) -> bool {
        self.reached_code_step.get()
    }

    /// The tokens, and whether this login needs a code to sign in. The shared
    /// flow has no outcome for "signed in without a code": `request_code` ends it
    /// with an error and leaves the tokens here for this to pick up.
    pub fn finish(
        &self,
        flow: &OtpLogin<'_, Self>,
        code: Option<&OtpCode>,
    ) -> Result<(Tokens, bool), CliError> {
        match flow.login(code) {
            Ok(tokens) => Ok((tokens, true)),
            Err(err) => match self.signed_without_code.borrow_mut().take() {
                Some(tokens) => Ok((tokens, false)),
                None => Err(err),
            },
        }
    }
}

impl<T: Transport> OtpTransport for SignIn<'_, T> {
    type Session = Tokens;

    fn request_code(&self) -> Result<CodeSent, CliError> {
        let (email, password) = self
            .login
            .ok_or_else(|| CliError::Other("a resumed sign-in cannot request a code".into()))?;
        let pkce = Pkce::generate()?;
        match block_on(begin(self.transport, email, password, &pkce))? {
            Begun::Signed(tokens) => {
                *self.signed_without_code.borrow_mut() = Some(tokens);
                Err(CliError::Other("signed in without a code".into()))
            }
            Begun::Code(pending) => {
                self.reached_code_step.set(true);
                let in_flight = serde_json::to_string(&pending)
                    .map_err(|e| CliError::Other(format!("parking the sign-in: {e}")))?;
                let mut sent = CodeSent::new(in_flight);
                sent.channel = pending.channel;
                sent.sent_to_hint = pending.sent_to_hint;
                Ok(sent)
            }
        }
    }

    fn redeem(&self, in_flight: &str, code: &OtpCode) -> Result<Tokens, CliError> {
        let pending: Pending = serde_json::from_str(in_flight).map_err(|_| {
            CliError::Auth(format!(
                "the waiting sign-in could not be read; run `{} auth login` for a new code",
                crate::BIN
            ))
        })?;
        block_on(redeem(self.transport, &pending, code.expose()))
    }
}

async fn token_request<T: Transport>(
    transport: &T,
    body: String,
) -> Result<HttpResponse, CliError> {
    let request = HttpRequest::new("POST", format!("{IDS}/connect/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body);
    transport.send(base(request)).await
}

/// `error` and `error_description` from a token endpoint's refusal, or the status.
fn token_refusal(response: &HttpResponse) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_str(&response.body).ok();
    let field = |name: &str| {
        parsed
            .as_ref()
            .and_then(|v| v.get(name))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    match (field("error"), field("error_description")) {
        (Some(e), Some(d)) => format!("HTTP {}: {e}: {d}", response.status),
        (Some(e), None) => format!("HTTP {}: {e}", response.status),
        _ => format!("HTTP {}", response.status),
    }
}

fn parse_tokens(response: &HttpResponse) -> Result<Tokens, CliError> {
    serde_json::from_str(&response.body).map_err(|_| {
        CliError::Upstream(format!(
            "myQ's token endpoint answered HTTP {} with no token",
            response.status
        ))
    })
}

/// The app also sends a `Firebase-AppCheck-Token` header here. This client
/// sends none: it has no attestation to offer and does not pretend otherwise.
async fn exchange<T: Transport>(
    transport: &T,
    code: &str,
    verifier: &str,
) -> Result<Tokens, CliError> {
    let body = form_body(&[
        ("client_id", CLIENT_ID),
        ("scope", SCOPE),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("code_verifier", verifier),
    ]);
    let response = token_request(transport, body).await?;
    if let Some(err) = refused(&response) {
        return Err(err);
    }
    if !response.is_success() {
        return Err(CliError::Upstream(format!(
            "myQ refused the sign-in code ({})",
            token_refusal(&response)
        )));
    }
    parse_tokens(&response)
}

/// Trade a refresh token for new tokens. `Auth` means that token is finished,
/// so the caller can fall back to the password; anything else is an outage.
pub async fn refresh<T: Transport>(transport: &T, refresh_token: &str) -> Result<Tokens, CliError> {
    let body = form_body(&[
        ("client_id", CLIENT_ID),
        ("scope", SCOPE),
        ("redirect_uri", REDIRECT_URI),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ]);
    let response = token_request(transport, body).await?;
    if let Some(err) = refused(&response) {
        return Err(err);
    }
    match response.status {
        200..=299 => parse_tokens(&response),
        400 | 401 => Err(CliError::Auth(format!(
            "myQ refused the saved session ({})",
            token_refusal(&response)
        ))),
        _ => Err(CliError::Upstream(format!(
            "myQ's token endpoint answered {}",
            token_refusal(&response)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::testing::{header_of, Scripted};
    use pk_cli_auth::otp::{MemorySlot, ParkingSlot};

    const LOGIN_PAGE: &str = include_str!("../tests/fixtures/login_page.html");
    const CODE_STEP: &str = include_str!("../tests/fixtures/code_step.html");
    const TOKEN: &str = include_str!("../tests/fixtures/token.json");

    fn pkce() -> Pkce {
        Pkce::from_random(&[7u8; 32])
    }

    async fn login<T: Transport>(
        transport: &T,
        email: &str,
        password: &str,
        pkce: &Pkce,
    ) -> Result<Tokens, CliError> {
        match begin(transport, email, password, pkce).await? {
            Begun::Signed(tokens) => Ok(tokens),
            Begun::Code(pending) => panic!("unexpected code step: {pending:?}"),
        }
    }

    fn to_code_step() -> Vec<HttpResponse> {
        vec![
            redirect("/Account/Login?ReturnUrl=x").header("set-cookie", "sid=dummy1; Path=/"),
            HttpResponse::new(200, LOGIN_PAGE),
            HttpResponse::new(200, CODE_STEP).header("set-cookie", "mfa=dummy2; Path=/"),
        ]
    }

    #[test]
    fn the_code_step_is_read_from_the_page() {
        let step = parse_code_step(CODE_STEP).expect("a code step");
        assert_eq!(step.field, "Otp");
        assert_eq!(step.action, "/Account/DummyVerify?returnUrl=x");
        assert_eq!(step.channel.as_deref(), Some("Email"));
        assert_eq!(step.sent_to_hint.as_deref(), Some("d***@example.com"));
        let names: Vec<&str> = step.fields.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"__RequestVerificationToken"));
        assert!(names.contains(&"SelectedMfaMethod"));
        assert!(!names.contains(&"Otp"), "the code field is filled in later");
        assert!(!names.contains(&"Unticked"), "{names:?}");
        assert!(parse_code_step(LOGIN_PAGE).is_none());
    }

    #[test]
    fn a_form_posts_where_a_browser_would_post_it() {
        let page = "https://partner-identity.myq-cloud.com/Account/Step?a=1";
        let at = |action| form_target(action, page).unwrap();
        assert_eq!(at(""), page);
        assert_eq!(
            at("?b=2"),
            "https://partner-identity.myq-cloud.com/Account/Step?b=2"
        );
        assert_eq!(at("/Other"), "https://partner-identity.myq-cloud.com/Other");
        assert_eq!(
            at("Verify"),
            "https://partner-identity.myq-cloud.com/Account/Verify"
        );
        assert!(form_target("https://evil.example/x", page).is_err());
    }

    #[test]
    fn the_code_finishes_the_session_that_asked_for_it() {
        let mut script = to_code_step();
        script.extend([
            redirect("com.myqops://android?code=dummycode"),
            HttpResponse::new(200, TOKEN),
        ]);
        let scripted = Scripted::new(script);
        let Begun::Code(pending) = block_on(begin(
            &scripted,
            "dummy@example.invalid",
            "dummy-pass",
            &pkce(),
        ))
        .unwrap() else {
            panic!("expected the code step");
        };
        // The parked form survives the keychain round trip.
        let parked = serde_json::to_string(&pending).unwrap();
        let pending: Pending = serde_json::from_str(&parked).unwrap();

        let tokens = block_on(redeem(&scripted, &pending, "123456")).unwrap();
        assert_eq!(tokens.access_token, "dummy-access-token");

        let sent = scripted.sent();
        let post = &sent[3];
        assert_eq!(
            post.url,
            "https://partner-identity.myq-cloud.com/Account/DummyVerify?returnUrl=x"
        );
        let body = post.body.as_deref().unwrap();
        assert!(body.contains("Otp=123456"), "{body}");
        assert!(body.contains("__RequestVerificationToken=dummy-step-token"));
        assert!(!body.contains("dummy-pass"));
        assert_eq!(
            header_of(post, "cookie").as_deref(),
            Some("sid=dummy1; mfa=dummy2")
        );
        let exchange = sent[4].body.as_deref().unwrap();
        assert!(exchange.contains(&format!("code_verifier={}", pkce().verifier)));
    }

    #[test]
    fn a_refused_code_is_an_auth_error_without_the_code() {
        let mut script = to_code_step();
        script.push(HttpResponse::new(200, CODE_STEP));
        let scripted = Scripted::new(script);
        let Begun::Code(pending) = block_on(begin(
            &scripted,
            "dummy@example.invalid",
            "dummy-pass",
            &pkce(),
        ))
        .unwrap() else {
            panic!("expected the code step");
        };
        let err = block_on(redeem(&scripted, &pending, "123456")).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("refused"), "{err}");
        assert!(!err.to_string().contains("123456"));
    }

    #[test]
    fn the_shared_flow_parks_the_sign_in_and_resumes_it_with_the_code() {
        let scripted = Scripted::new(to_code_step());
        let slot = MemorySlot::default();
        let signin = SignIn::start(&scripted, "dummy@example.invalid", "dummy-pass");
        let flow = OtpLogin::new("myq", &signin, &slot).quiet(true);
        let err = signin.finish(&flow, None).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("--code"), "{err}");
        assert!(signin.reached_code_step());
        let parked = slot.load().unwrap().expect("parked");
        assert_eq!(parked.sent_to_hint.as_deref(), Some("d***@example.com"));

        let scripted = Scripted::new(vec![
            redirect("com.myqops://android?code=dummycode"),
            HttpResponse::new(200, TOKEN),
        ]);
        let signin = SignIn::resume(&scripted);
        let flow = OtpLogin::new("myq", &signin, &slot).quiet(true);
        let code = OtpCode::parse("123456").unwrap();
        let (tokens, code_required) = signin.finish(&flow, Some(&code)).unwrap();
        assert_eq!(tokens.access_token, "dummy-access-token");
        assert!(code_required);
        assert!(slot.load().unwrap().is_none(), "cleared on success");
    }

    #[test]
    fn signing_in_without_a_code_is_a_success_and_parks_nothing() {
        let scripted = Scripted::new(vec![
            redirect("/Account/Login?ReturnUrl=x"),
            HttpResponse::new(200, LOGIN_PAGE),
            redirect("com.myqops://android?code=dummycode"),
            HttpResponse::new(200, TOKEN),
        ]);
        let slot = MemorySlot::default();
        let signin = SignIn::start(&scripted, "dummy@example.invalid", "dummy-pass");
        let flow = OtpLogin::new("myq", &signin, &slot).quiet(true);
        let (_, code_required) = signin.finish(&flow, None).unwrap();
        assert!(!code_required);
        assert!(slot.load().unwrap().is_none());
    }

    #[test]
    fn the_challenge_is_s256_of_the_verifier() {
        // RFC 7636 appendix B's vector.
        let random = [
            116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212,
            37, 77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141, 121,
        ];
        let pkce = Pkce::from_random(&random);
        assert_eq!(pkce.verifier, "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_authorize_url_carries_the_apps_parameters() {
        let url = authorize_url("CHALLENGE");
        assert!(url.starts_with("https://partner-identity.myq-cloud.com/connect/authorize?"));
        assert!(url.contains("client_id=ANDROID_CGI_MYQ"));
        assert!(url.contains("scope=MyQ_Residential%20offline_access"));
        assert!(url.contains("redirect_uri=com.myqops%3A%2F%2Fandroid"));
        assert!(url.contains("code_challenge=CHALLENGE"));
        assert!(url.contains("acr_values=unified_flow%3Av1%20%20brand%3Amyq"));
    }

    #[test]
    fn the_login_form_is_read_from_the_page() {
        let form = parse_login_form(LOGIN_PAGE).expect("a form");
        assert!(form.action.starts_with("/Account/LoginWithEmail"));
        let names: Vec<&str> = form.hidden.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"__RequestVerificationToken"));
        assert!(names.contains(&"ReturnUrl"));
        assert!(names.contains(&"Brand"));
    }

    #[test]
    fn a_page_without_a_password_field_has_no_login_form() {
        assert!(
            parse_login_form("<form action=\"/x\"><input type=\"text\" name=\"q\"></form>")
                .is_none()
        );
        assert!(parse_login_form("<html></html>").is_none());
    }

    #[test]
    fn the_code_is_read_and_decoded_from_the_custom_scheme_redirect() {
        assert_eq!(
            code_from_redirect("com.myqops://android?code=AB%2BCD&scope=x&iss=y").as_deref(),
            Some("AB+CD")
        );
        assert_eq!(
            code_from_redirect("com.myqops://android?error=denied"),
            None
        );
        assert_eq!(code_from_redirect("com.myqops://android"), None);
    }

    #[test]
    fn percent_decoding_handles_a_trailing_escape() {
        assert_eq!(percent_decode("abc%"), "abc%");
        assert_eq!(percent_decode("abc%2"), "abc%2");
        assert_eq!(percent_decode("a%20b"), "a b");
    }

    fn redirect(location: &str) -> HttpResponse {
        HttpResponse::new(302, "").header("location", location)
    }

    #[test]
    fn login_walks_the_whole_flow_and_keeps_its_cookies() {
        let scripted = Scripted::new(vec![
            redirect("/Account/Login?ReturnUrl=x").header("set-cookie", "sid=dummy1; Path=/"),
            HttpResponse::new(200, LOGIN_PAGE).header("set-cookie", "anti=dummy2; Path=/"),
            redirect("/connect/authorize/callback?x=1"),
            redirect("com.myqops://android?code=dummycode&scope=x"),
            HttpResponse::new(200, TOKEN),
        ]);
        let tokens = block_on(login(
            &scripted,
            "dummy@example.invalid",
            "dummy-pass",
            &pkce(),
        ))
        .unwrap();
        assert_eq!(tokens.access_token, "dummy-access-token");
        assert_eq!(tokens.refresh_token.as_deref(), Some("dummy-refresh-token"));

        let sent = scripted.sent();
        assert_eq!(sent.len(), 5);
        let post = &sent[2];
        assert_eq!(post.method, "POST");
        assert!(post.url.contains("/Account/LoginWithEmail"));
        let body = post.body.as_deref().unwrap();
        assert!(body.contains("Email=dummy%40example.invalid"));
        assert!(body.contains("Password=dummy-pass"));
        assert!(body.contains("__RequestVerificationToken=dummy-antiforgery"));
        assert_eq!(
            header_of(post, "cookie").as_deref(),
            Some("sid=dummy1; anti=dummy2")
        );

        let token = &sent[4];
        assert_eq!(
            token.url,
            "https://partner-identity.myq-cloud.com/connect/token"
        );
        let body = token.body.as_deref().unwrap();
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code=dummycode"));
        assert!(body.contains(&format!("code_verifier={}", pkce().verifier)));
        assert!(header_of(token, "firebase-appcheck-token").is_none());
    }

    #[test]
    fn a_rejected_password_is_an_auth_error_that_warns_about_lockout() {
        let scripted = Scripted::new(vec![
            redirect("/Account/Login?ReturnUrl=x"),
            HttpResponse::new(200, LOGIN_PAGE),
            HttpResponse::new(200, LOGIN_PAGE),
        ]);
        let err = block_on(login(
            &scripted,
            "dummy@example.invalid",
            "dummy-pass",
            &pkce(),
        ))
        .unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("lock the account"), "{err}");
        assert!(!err.to_string().contains("dummy-pass"));
    }

    #[test]
    fn a_further_step_names_its_fields_without_their_values() {
        let step = "<form action=\"/Account/Code\"><input type=\"text\" name=\"Code\" value=\"dummy9\">\
                    <input type=\"hidden\" name=\"__RequestVerificationToken\" value=\"dummy8\"></form>";
        let scripted = Scripted::new(vec![
            redirect("/Account/Login?ReturnUrl=x"),
            HttpResponse::new(200, LOGIN_PAGE),
            HttpResponse::new(200, step),
        ]);
        let err = block_on(login(
            &scripted,
            "dummy@example.invalid",
            "dummy-pass",
            &pkce(),
        ))
        .unwrap_err();
        assert_eq!(err.exit_code(), 3);
        let text = err.to_string();
        assert!(text.contains("Code"), "{text}");
        assert!(text.contains("/Account/Code"), "{text}");
        assert!(
            !text.contains("dummy9") && !text.contains("dummy8"),
            "{text}"
        );
    }

    #[test]
    fn a_redirect_off_the_identity_host_is_refused_and_not_followed() {
        let scripted = Scripted::new(vec![redirect("https://evil.example/steal")]);
        let err = block_on(login(
            &scripted,
            "dummy@example.invalid",
            "dummy-pass",
            &pkce(),
        ))
        .unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("evil.example"));
        assert_eq!(scripted.sent().len(), 1);
    }

    #[test]
    fn a_cloudflare_challenge_is_an_upstream_error() {
        let scripted = Scripted::new(vec![HttpResponse::new(403, "Just a moment...")]);
        let err = block_on(login(
            &scripted,
            "dummy@example.invalid",
            "dummy-pass",
            &pkce(),
        ))
        .unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("Cloudflare"));
    }

    #[test]
    fn a_refused_code_reports_the_servers_reason() {
        let scripted = Scripted::new(vec![
            redirect("com.myqops://android?code=dummycode"),
            HttpResponse::new(
                400,
                r#"{"error":"invalid_grant","error_description":"dummy reason"}"#,
            ),
        ]);
        let err = block_on(login(
            &scripted,
            "dummy@example.invalid",
            "dummy-pass",
            &pkce(),
        ))
        .unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(
            err.to_string().contains("invalid_grant: dummy reason"),
            "{err}"
        );
    }

    #[test]
    fn refresh_sends_the_apps_grant_and_maps_a_dead_token_to_auth() {
        let scripted = Scripted::new(vec![HttpResponse::new(200, TOKEN)]);
        block_on(refresh(&scripted, "dummy-old")).unwrap();
        let body = scripted.sent()[0].body.clone().unwrap();
        assert!(body.contains("grant_type=refresh_token"));
        assert!(body.contains("refresh_token=dummy-old"));
        assert!(body.contains("client_id=ANDROID_CGI_MYQ"));

        let scripted = Scripted::new(vec![HttpResponse::new(400, r#"{"error":"invalid_grant"}"#)]);
        assert_eq!(
            block_on(refresh(&scripted, "dummy-old"))
                .unwrap_err()
                .exit_code(),
            3
        );

        let scripted = Scripted::new(vec![HttpResponse::new(503, "")]);
        assert_eq!(
            block_on(refresh(&scripted, "dummy-old"))
                .unwrap_err()
                .exit_code(),
            5
        );
    }
}
