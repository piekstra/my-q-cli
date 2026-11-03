//! myQ's JSON → the DTOs this CLI prints. A door is any device whose state
//! carries `door_state`: that field is what the app itself reads.

use std::time::Duration;

use pk_cli_core::{resolve, CliError};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// How long a door command waits for the read-back to show the target state.
#[derive(Debug, Clone, Copy)]
pub struct Wait {
    pub timeout: Duration,
    pub interval: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorCommand {
    Open,
    Close,
}

impl DoorCommand {
    pub fn path(self) -> &'static str {
        match self {
            DoorCommand::Open => "open",
            DoorCommand::Close => "close",
        }
    }

    /// The `door_state` that proves the command worked.
    pub fn target(self) -> &'static str {
        match self {
            DoorCommand::Open => "open",
            DoorCommand::Close => "closed",
        }
    }

    /// The `door_state` of a door already on its way to the target. Some openers
    /// take a second command as stop or reverse, so one is never sent over this.
    pub fn moving(self) -> &'static str {
        match self {
            DoorCommand::Open => "opening",
            DoorCommand::Close => "closing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Account {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Door {
    pub name: String,
    /// Absent for a door read from the phone's screen, which does not show one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// myQ's own `door_state`, e.g. `open`, `closed`, `opening`, `closing`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update: Option<String>,
    /// The phone app's own wording, e.g. `Closed for 14 minutes`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Device {
    pub name: String,
    pub serial: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub door_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lamp_state: Option<String>,
}

/// The outcome of a door command, from a read-back rather than the write's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DoorChange {
    pub door: Door,
    /// False when no command was sent: the door was already in the requested state,
    /// or already moving toward it.
    pub changed: bool,
    /// True when a read-back showed the requested state.
    pub confirmed: bool,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn items<'a>(value: &'a Value, keys: &[&str]) -> &'a [Value] {
    if let Some(array) = value.as_array() {
        return array;
    }
    keys.iter()
        .find_map(|k| value.get(*k).and_then(Value::as_array))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn accounts_from(value: &Value) -> Vec<Account> {
    items(value, &["accounts", "items"])
        .iter()
        .filter_map(|a| {
            Some(Account {
                id: text(a, "id")?,
                name: text(a, "name").unwrap_or_default(),
            })
        })
        .collect()
}

pub fn devices_from(value: &Value) -> Vec<Device> {
    items(value, &["items"])
        .iter()
        .filter_map(|d| {
            let state = d.get("state");
            let of_state = |key: &str| state.and_then(|s| text(s, key));
            Some(Device {
                serial: text(d, "serial_number")?,
                name: text(d, "name").unwrap_or_default(),
                family: text(d, "device_family"),
                device_type: text(d, "device_type"),
                parent: text(d, "parent_device_id"),
                online: state.and_then(|s| s.get("online")).and_then(Value::as_bool),
                door_state: of_state("door_state"),
                lamp_state: of_state("lamp_state"),
            })
        })
        .collect()
}

pub fn doors_from(value: &Value) -> Vec<Door> {
    items(value, &["items"])
        .iter()
        .filter_map(|d| {
            let state = d.get("state")?;
            Some(Door {
                name: text(d, "name").unwrap_or_default(),
                serial: Some(text(d, "serial_number")?),
                state: text(state, "door_state")?,
                online: state.get("online").and_then(Value::as_bool),
                last_update: text(state, "last_update"),
                status: None,
            })
        })
        .collect()
}

/// A door by name, serial, or a unique part of its name. A tie is exit 4 naming
/// the candidates: guessing wrong here moves the wrong door.
pub fn find<'a>(doors: &'a [Door], query: &str) -> Result<&'a Door, CliError> {
    resolve::pick(
        doors,
        query,
        |d| d.serial.iter().cloned().collect(),
        |d| &d.name,
        "door",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/devices.json")).unwrap()
    }

    #[test]
    fn only_devices_with_a_door_state_are_doors() {
        let doors = doors_from(&devices());
        assert_eq!(doors.len(), 2);
        assert_eq!(doors[0].name, "Dummy Left Door");
        assert_eq!(doors[0].state, "closed");
        assert_eq!(doors[0].online, Some(true));
    }

    #[test]
    fn a_shared_word_is_an_ambiguity_and_never_a_silent_pick() {
        let doors = doors_from(&devices());
        assert_eq!(find(&doors, "left").unwrap().name, "Dummy Left Door");
        assert_eq!(
            find(&doors, "DUMMYSERIAL02").unwrap().name,
            "Dummy Right Door"
        );
        let err = find(&doors, "door").unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert!(err.to_string().contains("more than one"));
        assert_eq!(find(&doors, "shed").unwrap_err().exit_code(), 4);
    }
}
