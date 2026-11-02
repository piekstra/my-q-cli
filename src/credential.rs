//! The stored credential: one keychain item holding the login and its tokens
//! (DESIGN.md §1.7), plus the seam that lets tests run without a keychain.

use std::fmt;

use pk_cli_auth::otp::{ParkedLogin, ParkingSlot};
use pk_cli_core::CliError;
use pk_cli_secrets::CredentialStore;
use serde::{Deserialize, Serialize};

pub const KEYCHAIN_ACCOUNT: &str = "credentials";
pub const ENV_EMAIL: &str = "MYQ_EMAIL";
pub const ENV_PASSWORD: &str = "MYQ_PASSWORD";
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
    /// This login needs a one-time code, so the stored password alone cannot sign
    /// in, and trying would only make myQ send another code.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub code_required: bool,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("password", &self.password.is_some())
            .field("access_token", &self.access_token.is_some())
            .field("refresh_token", &self.refresh_token.is_some())
            .field("expires_at", &self.expires_at)
            .field("pending", &self.pending.is_some())
            .field("code_required", &self.code_required)
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

/// `MYQ_EMAIL` and `MYQ_PASSWORD` first (§1.7: env before keychain), then the
/// keychain item. In env mode nothing is written: a `.mcpb` runs without a
/// keychain, and an MCP tool must never create an item.
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

    pub fn from_env() -> Option<Credentials> {
        let email = std::env::var(ENV_EMAIL).ok().filter(|v| !v.is_empty())?;
        let password = std::env::var(ENV_PASSWORD).ok().filter(|v| !v.is_empty())?;
        Some(Credentials {
            email,
            password: Some(password),
            ..Credentials::default()
        })
    }

    /// The keychain item alone, ignoring the environment.
    pub fn load_item(&self) -> Result<Option<Credentials>, CliError> {
        self.store.get_json(KEYCHAIN_ACCOUNT)
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
        match Self::from_env() {
            Some(credentials) => Ok(Some(credentials)),
            None => self.load_item(),
        }
    }

    fn save(&self, credentials: &Credentials) -> Result<(), CliError> {
        if Self::from_env().is_some() {
            return Ok(());
        }
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
        slot.clear().unwrap();
        let item = store.0.borrow().clone().unwrap();
        assert!(item.pending.is_none());
        assert_eq!(item.password.as_deref(), Some("dummy-password"));
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
