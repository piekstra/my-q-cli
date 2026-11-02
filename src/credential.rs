//! The stored credential: one keychain item holding the login and its tokens
//! (DESIGN.md §1.7), plus the seam that lets tests run without a keychain.

use std::fmt;

use pk_cli_auth::otp::{ParkedLogin, ParkingSlot};
use pk_cli_core::CliError;
use pk_cli_secrets::CredentialStore;
use serde::{Deserialize, Serialize};

use crate::auth::Tokens;
use crate::client::unix_now;

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
    pub fn apply_tokens(&mut self, tokens: Tokens) {
        self.access_token = Some(tokens.access_token);
        if tokens.refresh_token.is_some() {
            self.refresh_token = tokens.refresh_token;
        }
        let lifetime = tokens
            .expires_in
            .unwrap_or(DEFAULT_LIFETIME_SECS)
            .min(MAX_LIFETIME_SECS);
        self.expires_at = Some(unix_now().saturating_add(lifetime));
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
    fn a_token_response_without_a_lifetime_gets_the_default() {
        let mut item = Credentials::default();
        item.apply_tokens(tokens(Some("dummy-refresh"), None));
        let left = item.expires_at.unwrap() - unix_now();
        assert!((1790..=1800).contains(&left), "{left}");
    }

    #[test]
    fn a_wild_lifetime_is_capped() {
        let mut item = Credentials::default();
        item.apply_tokens(tokens(None, Some(u64::MAX)));
        assert!(item.expires_at.unwrap() <= unix_now() + MAX_LIFETIME_SECS);
        assert!(item.expires_at.unwrap() > unix_now());
    }

    #[test]
    fn a_response_without_a_refresh_token_keeps_the_old_one() {
        let mut item = Credentials {
            refresh_token: Some("dummy-old".into()),
            ..Credentials::default()
        };
        item.apply_tokens(tokens(None, Some(60)));
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
