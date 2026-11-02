//! The Claude Desktop extension manifest (`packaging/mcpb/manifest.json`) against
//! the code it packages, so a release cannot ship a manifest that names a tool or
//! a version the binary does not have. The release workflow validates the schema
//! with the `mcpb` CLI.

use myq::door::{Door, DoorChange, DoorCommand};
use myq::mcp::{MyqTools, Source};
use pk_cli_core::CliError;
use pk_cli_mcp::Toolset;
use serde_json::Value;

fn manifest() -> Value {
    let path = format!(
        "{}/packaging/mcpb/manifest.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

struct NoSource;

impl Source for NoSource {
    async fn doors(&self) -> Result<Vec<Door>, CliError> {
        unreachable!()
    }
    async fn set_door(&self, _: &str, _: DoorCommand) -> Result<DoorChange, CliError> {
        unreachable!()
    }
}

#[test]
fn the_manifest_version_is_the_crate_version() {
    assert_eq!(manifest()["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn the_manifest_lists_exactly_the_served_tools() {
    let listed: Vec<String> = manifest()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let served: Vec<String> = MyqTools { source: NoSource }
        .tools()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(listed, served);
}

/// myQ's sign-in can need a one-time code, which an extension's settings cannot
/// answer, so the extension reads the keychain session and takes no password.
#[test]
fn every_platform_runs_mcp_and_no_setting_carries_a_credential() {
    let m = manifest();
    let config = &m["server"]["mcp_config"];
    let mut configs = vec![config.clone()];
    for os in ["win32", "linux"] {
        configs.push(config["platform_overrides"][os].clone());
    }
    for c in &configs {
        assert_eq!(c["args"], serde_json::json!(["mcp"]), "{c}");
        assert!(
            c["command"]
                .as_str()
                .unwrap()
                .starts_with("${__dirname}/server/"),
            "{c}"
        );
        assert!(c.get("env").is_none(), "{c}");
    }
    assert!(m.get("user_config").is_none());
}
