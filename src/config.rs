//! Non-secret settings, stored at `~/.config/myq/config.toml`.

use pk_cli_core::CliError;
use pk_cli_secrets::OpRef;
use serde::{Deserialize, Serialize};

pub const KEYS: &str = "email, account, hub_host, op_ref, op_email_ref";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// The myQ account id to act on, when the login has more than one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// LAN address of the opener's Wi-Fi hub, for `hub status`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub_host: Option<String>,
    /// Where 1Password keeps the password (`op://vault/item/field`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op_ref: Option<OpRef>,
    /// Where 1Password keeps the email (`op://vault/item/field`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op_email_ref: Option<OpRef>,
}

impl Config {
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), CliError> {
        match key {
            "email" => self.email = Some(value.to_string()),
            "account" => self.account = Some(value.to_string()),
            "hub_host" => self.hub_host = Some(value.to_string()),
            "op_ref" => self.op_ref = Some(value.parse().map_err(CliError::Usage)?),
            "op_email_ref" => self.op_email_ref = Some(value.parse().map_err(CliError::Usage)?),
            other => return Err(unknown(other)),
        }
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> Result<(), CliError> {
        match key {
            "email" => self.email = None,
            "account" => self.account = None,
            "hub_host" => self.hub_host = None,
            "op_ref" => self.op_ref = None,
            "op_email_ref" => self.op_email_ref = None,
            other => return Err(unknown(other)),
        }
        Ok(())
    }
}

fn unknown(key: &str) -> CliError {
    CliError::Usage(format!("unknown config key `{key}` (known: {KEYS})"))
}
