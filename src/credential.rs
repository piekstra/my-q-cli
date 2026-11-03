//! The stored credential: one keychain item holding the login and its tokens
//! (DESIGN.md §1.7), plus the seam that lets tests run without a keychain.

use std::fmt;

use pk_cli_auth::otp::{ParkedLogin, ParkingSlot};
use pk_cli_core::CliError;
use pk_cli_secrets::CredentialStore;
use serde::{Deserialize, Serialize};

use crate::auth::Tokens;

/// What a token response with no `expires_in` is taken to last.
const DEFAULT_LIFETIME_SECS: u64 = 1800;
/// A server's `expires_in` is believed up to this, so a wild one cannot overflow.
const MAX_LIFETIME_SECS: u64 = 365 * 24 * 60 * 60;

pub const KEYCHAIN_ACCOUNT: &str = "credentials";
pub const ENV_EMAIL: &str = "MYQ_EMAIL";
pub const ENV_KEYCHAIN_SERVICE: &str = "MYQ_KEYCHAIN_SERVICE";

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds when the access token stops working.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// A sign-in waiting on its one-time code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<ParkedLogin>,
    /// Posting the stored password cannot sign in: myQ refused it or answers it with
    /// a step only a person can finish. Each post can cost a one-time code or count
    /// toward a lockout, so none is made until `auth login` replaces this item.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub password_blocked: bool,
}

impl Credentials {
    /// `now` is unix seconds.
    pub fn apply_tokens(&mut self, tokens: Tokens, now: u64) {
        self.access_token = Some(tokens.access_token);
        if tokens.refresh_token.is_some() {
            self.refresh_token = tokens.refresh_token;
        }
        let lifetime = tokens
            .expires_in
            .unwrap_or(DEFAULT_LIFETIME_SECS)
            .min(MAX_LIFETIME_SECS);
        self.expires_at = Some(now.saturating_add(lifetime));
    }

    fn clear_session(&mut self) {
        self.access_token = None;
        self.refresh_token = None;
        self.expires_at = None;
    }

    /// End the session and anything waiting. The password stays but is not posted,
    /// so no later command signs back in behind the person who signed out.
    pub fn sign_out(&mut self) {
        self.clear_session();
        self.pending = None;
        self.password_blocked = true;
    }

    pub fn has_session(&self) -> bool {
        self.access_token.is_some() || self.refresh_token.is_some()
    }

    /// Whether a session could be renewed without a person.
    pub fn renewable(&self) -> bool {
        self.refresh_token.is_some() || (self.password.is_some() && !self.password_blocked)
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("password", &self.password.is_some())
            .field("access_token", &self.access_token.is_some())
            .field("refresh_token", &self.refresh_token.is_some())
            .field("expires_at", &self.expires_at)
            .field("pending", &self.pending.is_some())
            .field("password_blocked", &self.password_blocked)
            .finish()
    }
}

/// Where a client reads credentials and writes back rotated tokens.
pub trait TokenStore {
    fn load(&self) -> Result<Option<Credentials>, CliError>;
    fn save(&self, credentials: &Credentials) -> Result<(), CliError>;
}

impl<S: TokenStore + ?Sized> TokenStore for &S {
    fn load(&self) -> Result<Option<Credentials>, CliError> {
        (**self).load()
    }
    fn save(&self, credentials: &Credentials) -> Result<(), CliError> {
        (**self).save(credentials)
    }
}

/// The parked sign-in, inside the one credential item (DESIGN.md §1.7). `seed` is
/// the email and password of a sign-in in progress: they join the item only once
/// myQ has accepted them, which is when the sign-in is first parked.
pub struct ParkedView<'a, S: TokenStore> {
    store: &'a S,
    seed: Option<Credentials>,
}

impl<'a, S: TokenStore> ParkedView<'a, S> {
    pub fn new(store: &'a S, seed: Option<Credentials>) -> Self {
        Self { store, seed }
    }
}

impl<S: TokenStore> ParkingSlot for ParkedView<'_, S> {
    fn load(&self) -> Result<Option<ParkedLogin>, CliError> {
        Ok(self.store.load()?.and_then(|c| c.pending))
    }

    fn save(&self, parked: &ParkedLogin) -> Result<(), CliError> {
        let mut item = self.store.load()?.unwrap_or_default();
        if let Some(seed) = &self.seed {
            // Another login's session must not answer for this one while it waits.
            if item.email != seed.email {
                item.clear_session();
            }
            item.email = seed.email.clone();
            item.password = seed.password.clone();
        }
        item.pending = Some(parked.clone());
        // Reaching a code step proves the password alone cannot sign in. A read
        // during the wait must not post it, or myQ could send a second code that
        // does not match the parked session.
        item.password_blocked = true;
        self.store.save(&item)
    }

    fn clear(&self) -> Result<(), CliError> {
        if let Some(mut item) = self.store.load()? {
            if item.pending.take().is_some() {
                self.store.save(&item)?;
            }
        }
        Ok(())
    }
}

/// The keychain item. DESIGN.md §1.7 would put environment variables first; this
/// CLI reads none. A session has to keep rotated tokens and what myQ said about the
/// password, or every process would post the password again, and each post can cost
/// a one-time code. `MYQ_EMAIL` and `auth login --from-env` remain sign-in inputs.
pub struct KeychainStore {
    store: CredentialStore,
}

impl KeychainStore {
    pub fn new() -> Self {
        let store = match std::env::var(ENV_KEYCHAIN_SERVICE) {
            Ok(service) if !service.is_empty() => CredentialStore::new(service),
            _ => CredentialStore::for_binary(crate::BIN),
        };
        Self { store }
    }

    pub fn delete(&self) -> Result<bool, CliError> {
        self.store.delete(KEYCHAIN_ACCOUNT)
    }

    /// The bearer token `myq mcp --new-token` writes. It is the server's own item, with no myQ
    /// credential in it, and a server already running keeps the copy it started with.
    pub fn delete_connector_token(&self) -> Result<bool, CliError> {
        self.store.delete(pk_cli_mcp::HTTP_TOKEN_ACCOUNT)
    }
}

/// What `auth logout --forget` owes whoever ran it, having removed a connector token: the door tools
/// use the phone and not the session, so a running server still answers once they are signed out.
/// Only macOS has one command to give, as with `pk_cli_mcp::restart_note`.
pub fn server_keeps_its_token() -> String {
    let bin = crate::BIN;
    let warning = format!(
        "`{bin} mcp --http` keeps the token it started with until it stops, and one started with \
         --allow-writes can still move a door."
    );
    if cfg!(target_os = "macos") {
        format!("{warning} To stop it: launchctl bootout gui/$(id -u)/com.piekstra.{bin}-mcp-http")
    } else {
        format!("{warning} To end that, stop the server.")
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenStore for KeychainStore {
    fn load(&self) -> Result<Option<Credentials>, CliError> {
        self.store.get_json(KEYCHAIN_ACCOUNT)
    }

    fn save(&self, credentials: &Credentials) -> Result<(), CliError> {
        self.store.set_json(KEYCHAIN_ACCOUNT, credentials)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logout_says_how_to_stop_a_server_that_can_still_move_a_door() {
        let note = server_keeps_its_token();
        assert!(note.contains("myq mcp --http"), "{note}");
        assert!(note.contains("--allow-writes"), "{note}");
        assert!(note.contains("can still move a door"), "{note}");
        assert_eq!(
            note.contains("launchctl bootout gui/$(id -u)/com.piekstra.myq-mcp-http"),
            cfg!(target_os = "macos"),
            "{note}"
        );
        assert_eq!(
            note.contains("launchctl"),
            cfg!(target_os = "macos"),
            "{note}"
        );
    }
    use crate::client::testing::Memory;

    #[test]
    fn the_parked_view_meets_the_slot_contract() {
        let store = Memory::default();
        pk_cli_auth::otp::check_slot_contract(&ParkedView::new(&store, None));
    }

    #[test]
    fn parking_adds_the_accepted_login_and_clearing_keeps_it() {
        let store = Memory::default();
        let seed = Credentials {
            email: "dummy@example.invalid".into(),
            password: Some("dummy-password".into()),
            ..Credentials::default()
        };
        let slot = ParkedView::new(&store, Some(seed));
        assert!(store.0.borrow().is_none(), "nothing stored before parking");
        slot.save(&ParkedLogin::new("dummy-session", 1)).unwrap();
        let item = store.0.borrow().clone().unwrap();
        assert_eq!(item.email, "dummy@example.invalid");
        assert!(item.pending.is_some());
        assert!(item.password_blocked, "the password must not be posted now");
        slot.clear().unwrap();
        let item = store.0.borrow().clone().unwrap();
        assert!(item.pending.is_none());
        assert_eq!(item.password.as_deref(), Some("dummy-password"));
    }

    fn tokens(refresh: Option<&str>, expires_in: Option<u64>) -> Tokens {
        Tokens {
            access_token: "dummy-access".into(),
            refresh_token: refresh.map(str::to_string),
            expires_in,
        }
    }

    #[test]
    fn parking_a_different_login_drops_the_old_session_and_the_same_login_keeps_it() {
        let session = Credentials {
            email: "old@example.invalid".into(),
            access_token: Some("dummy-access".into()),
            refresh_token: Some("dummy-refresh".into()),
            expires_at: Some(1),
            ..Credentials::default()
        };
        let seed = |email: &str| Credentials {
            email: email.into(),
            password: Some("dummy-password".into()),
            ..Credentials::default()
        };
        let parked = ParkedLogin::new("dummy-session", 1);

        let store = Memory(std::cell::RefCell::new(Some(session.clone())));
        let slot = ParkedView::new(&store, Some(seed("new@example.invalid")));
        slot.save(&parked).unwrap();
        let item = store.0.borrow().clone().unwrap();
        assert_eq!(item.email, "new@example.invalid");
        assert!(!item.has_session());

        let store = Memory(std::cell::RefCell::new(Some(session)));
        let slot = ParkedView::new(&store, Some(seed("old@example.invalid")));
        slot.save(&parked).unwrap();
        assert!(store.0.borrow().as_ref().unwrap().has_session());
    }

    #[test]
    fn signing_out_ends_every_way_back_in_without_a_person() {
        let mut item = Credentials {
            password: Some("dummy-password".into()),
            access_token: Some("dummy-access".into()),
            refresh_token: Some("dummy-refresh".into()),
            expires_at: Some(1),
            pending: Some(ParkedLogin::new("dummy-session", 1)),
            ..Credentials::default()
        };
        item.sign_out();
        assert!(!item.has_session() && !item.renewable());
        assert!(item.pending.is_none());
        assert_eq!(item.password.as_deref(), Some("dummy-password"));
    }

    #[test]
    fn a_token_response_without_a_lifetime_gets_the_default() {
        let mut item = Credentials::default();
        item.apply_tokens(tokens(Some("dummy-refresh"), None), 1_000);
        assert_eq!(item.expires_at, Some(1_000 + DEFAULT_LIFETIME_SECS));
    }

    #[test]
    fn a_wild_lifetime_is_capped() {
        let mut item = Credentials::default();
        item.apply_tokens(tokens(None, Some(u64::MAX)), 1_000);
        assert_eq!(item.expires_at, Some(1_000 + MAX_LIFETIME_SECS));
    }

    #[test]
    fn a_response_without_a_refresh_token_keeps_the_old_one() {
        let mut item = Credentials {
            refresh_token: Some("dummy-old".into()),
            ..Credentials::default()
        };
        item.apply_tokens(tokens(None, Some(60)), 1_000);
        assert_eq!(item.refresh_token.as_deref(), Some("dummy-old"));
    }

    #[test]
    fn only_a_usable_way_back_in_counts_as_renewable() {
        let mut item = Credentials {
            password: Some("dummy-password".into()),
            ..Credentials::default()
        };
        assert!(item.renewable());
        item.password_blocked = true;
        assert!(!item.renewable());
        item.refresh_token = Some("dummy-refresh".into());
        assert!(item.renewable());
    }

    #[test]
    fn debug_never_shows_a_secret() {
        let credentials = Credentials {
            email: "dummy@example.invalid".into(),
            password: Some("dummy-password".into()),
            access_token: Some("dummy-access".into()),
            refresh_token: Some("dummy-refresh".into()),
            expires_at: Some(1),
            ..Credentials::default()
        };
        let shown = format!("{credentials:?}");
        for secret in ["dummy-password", "dummy-access", "dummy-refresh", "dummy@"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }
}
