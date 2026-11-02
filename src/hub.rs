//! The opener's own web server on the home network. It only serves Wi-Fi setup
//! and link status: door control is not on it, so this module stays read-only.

use pk_cli_core::transport::{HttpRequest, Transport};
use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::Value;

pub const ENV_HOST: &str = "MYQ_HUB_HOST";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HubStatus {
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rssi: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

/// A host name or IP with an optional port, so nothing else joins the URL.
pub fn validate_host(host: &str) -> Result<(), CliError> {
    let fine = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    if fine {
        Ok(())
    } else {
        Err(CliError::Usage(format!(
            "`{host}` is not a host name or IP address"
        )))
    }
}

pub fn parse_sys(host: &str, value: &Value) -> Result<HubStatus, CliError> {
    let station = value
        .get("connection")
        .and_then(|c| c.get("station"))
        .ok_or_else(|| {
            CliError::Upstream(format!(
                "{host} answered, but not with a myQ hub's status: is that the right address?"
            ))
        })?;
    let text = |k: &str| station.get(k).and_then(Value::as_str).map(str::to_string);
    let number = |k: &str| station.get(k).and_then(Value::as_i64);
    Ok(HubStatus {
        host: host.to_string(),
        ssid: text("ssid"),
        rssi: number("rssi"),
        channel: number("channel"),
        ip: station
            .pointer("/ip/ipv4/ipaddr")
            .and_then(Value::as_str)
            .map(str::to_string),
        mac: text("mac_addr"),
    })
}

pub async fn status<T: Transport>(transport: &T, host: &str) -> Result<HubStatus, CliError> {
    let request =
        HttpRequest::get(format!("http://{host}/sys")).user_agent(crate::BIN, crate::VERSION);
    let response = transport.send(request).await?;
    if !response.is_success() {
        return Err(CliError::Upstream(format!(
            "{host} answered /sys with HTTP {}",
            response.status
        )));
    }
    let value: Value = serde_json::from_str(&response.body).map_err(|_| {
        CliError::Upstream(format!(
            "{host} answered, but not with a myQ hub's status: is that the right address?"
        ))
    })?;
    parse_sys(host, &value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hub_fixture_parses() {
        let value: Value =
            serde_json::from_str(include_str!("../tests/fixtures/hub_sys.json")).unwrap();
        let status = parse_sys("192.0.2.10", &value).unwrap();
        assert_eq!(status.ssid.as_deref(), Some("DummyNetwork"));
        assert_eq!(status.rssi, Some(-40));
        assert_eq!(status.ip.as_deref(), Some("192.0.2.10"));
    }

    #[test]
    fn another_devices_json_is_an_upstream_error_naming_the_address() {
        let err = parse_sys("192.0.2.10", &serde_json::json!({"hello": 1})).unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("192.0.2.10"));
    }

    #[test]
    fn only_a_bare_host_is_accepted() {
        assert!(validate_host("192.0.2.10").is_ok());
        assert!(validate_host("hub.local:8080").is_ok());
        for bad in ["", "http://x", "a/b", "a b", "x?y=1"] {
            assert_eq!(validate_host(bad).unwrap_err().exit_code(), 2, "{bad}");
        }
    }
}
