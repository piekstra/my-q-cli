//! Black-box tests of the command surface: help, exit codes, the confirmation
//! gate. Nothing here reaches a keychain item or the network: `MYQ_KEYCHAIN_SERVICE`
//! points at an empty service, and every case fails or answers before a request.

use assert_cmd::Command;
use predicates::str::contains;

fn myq() -> Command {
    let mut cmd = Command::cargo_bin("myq").expect("binary builds");
    cmd.env("MYQ_KEYCHAIN_SERVICE", "piekstra.myq.hermetic-tests")
        .env_remove("MYQ_EMAIL")
        .env_remove("MYQ_ACCOUNT")
        .env_remove("MYQ_HUB_HOST")
        // A connected phone must be out of every test's reach: a tap moves a door.
        .env("MYQ_ADB", "/nonexistent/adb")
        .env(
            "XDG_CONFIG_HOME",
            std::env::temp_dir().join("myq-tests-empty-config"),
        );
    cmd
}

/// Signed-out commands read the keychain. Linux CI has no keychain service, and a
/// rebuilt test binary is a new identity to macOS, so these run only when asked.
fn keychain_tests_enabled() -> bool {
    std::env::var_os("MYQ_TEST_KEYCHAIN").is_some()
}

const COMMANDS: &[&str] = &[
    "auth",
    "config",
    "accounts",
    "devices",
    "doors",
    "hub",
    "api",
    "mcp",
    "self-update",
    "completions",
    "info",
];

const SUBCOMMANDS: &[&[&str]] = &[
    &["auth", "login"],
    &["auth", "status"],
    &["auth", "logout"],
    &["auth", "set-credential"],
    &["config", "path"],
    &["config", "show"],
    &["config", "set"],
    &["config", "unset"],
    &["accounts", "list"],
    &["accounts", "use"],
    &["devices", "list"],
    &["doors", "list"],
    &["doors", "get"],
    &["doors", "open"],
    &["doors", "close"],
    &["hub", "status"],
];

#[test]
fn top_level_help_lists_every_command() {
    let out = myq().arg("--help").assert().success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    for cmd in COMMANDS {
        assert!(text.contains(cmd), "--help is missing `{cmd}`");
    }
}

#[test]
fn every_command_help_renders() {
    // Rendering help runs clap's debug assertions over that subtree, which
    // catches a flag colliding with a global like `-a` or `-q`.
    for cmd in COMMANDS {
        myq().args([cmd, "--help"]).assert().success();
    }
    for path in SUBCOMMANDS {
        myq().args(*path).arg("--help").assert().success();
    }
}

#[test]
fn list_commands_answer_to_ls() {
    for group in ["accounts", "devices", "doors"] {
        myq().args([group, "ls", "--help"]).assert().success();
    }
}

#[test]
fn info_is_a_cli_info_dto() {
    let out = myq().arg("info").assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["name"], "myq");
    assert_eq!(v["spec"], "piekstra-cli/1");
    assert_eq!(v["auth"]["login_hint"], "myq auth login");
}

#[test]
fn completions_render() {
    myq()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(contains("myq"));
}

#[test]
fn moving_a_door_without_force_exits_6_before_touching_anything() {
    for verb in ["open", "close"] {
        let out = myq()
            .args(["--json", "doors", verb, "Left"])
            .assert()
            .code(6);
        let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v["error"]["code"], "confirmation_required");
    }
}

#[test]
fn doors_without_adb_name_what_to_install() {
    let out = myq().args(["--json", "doors", "list"]).assert().code(5);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("platform-tools"));
}

#[test]
fn a_write_through_api_without_force_exits_6() {
    myq()
        .args(["--json", "api", "PUT", "/api/v6.0/x"])
        .assert()
        .code(6);
}

#[test]
fn api_refuses_to_send_the_token_anywhere_else() {
    for url in [
        "https://evil.example/x",
        "http://devices.myq-cloud.com/x",
        "https://myq-cloud.com.evil.example/x",
        "https://evil.example\\.myq-cloud.com/x",
        "https://devices.myq-cloud.com@evil.example/x",
    ] {
        myq().args(["--json", "api", "GET", url]).assert().code(2);
    }
}

#[test]
fn signed_out_reads_exit_3_and_name_the_login_command() {
    if !keychain_tests_enabled() {
        return;
    }
    let out = myq()
        .args(["--json", "doors", "list", "--cloud"])
        .assert()
        .code(3);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["error"]["code"], "auth");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("myq auth login"));
}

#[test]
fn signed_out_status_is_a_dto_rather_than_an_error() {
    if !keychain_tests_enabled() {
        return;
    }
    let out = myq().args(["--json", "auth", "status"]).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "auth-status/v1");
    assert_eq!(v["authenticated"], false);
    assert_eq!(v["credential_in_keychain"], false);
}

#[test]
fn login_without_an_email_is_a_usage_error_before_any_password() {
    myq()
        .args(["auth", "login", "--non-interactive"])
        .assert()
        .code(2);
}

#[test]
fn a_secret_cannot_be_passed_as_an_argument() {
    myq()
        .args(["auth", "login", "--password", "x"])
        .assert()
        .code(2);
}

#[test]
fn hub_status_needs_an_address_and_refuses_a_url() {
    myq().args(["hub", "status"]).assert().code(2);
    myq()
        .args(["hub", "status", "--host", "http://x/y"])
        .assert()
        .code(2);
}

#[test]
fn unknown_config_key_is_a_usage_error() {
    let dir = std::env::temp_dir().join(format!("myq-config-test-{}", std::process::id()));
    let path = dir.join("config.toml");
    let path = path.to_str().unwrap();
    myq()
        .args(["--config", path, "config", "set", "nope", "1"])
        .assert()
        .code(2);
    myq()
        .args(["--config", path, "config", "set", "hub_host", "192.0.2.10"])
        .assert()
        .success();
    myq()
        .args(["--config", path, "--json", "config", "show"])
        .assert()
        .success()
        .stdout(contains("192.0.2.10"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn mcp_lists_its_tools_without_a_credential() {
    let out = myq().args(["mcp", "--list-tools"]).assert().success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    for tool in ["myq_doors", "myq_door_open", "myq_door_close"] {
        assert!(text.contains(tool), "{tool}");
    }
}

#[test]
fn a_malformed_code_is_usage_before_any_keychain_read() {
    myq()
        .args(["auth", "login", "--code", "12"])
        .assert()
        .code(2)
        .stderr(contains("verification code"));
}

#[test]
fn a_code_takes_no_password_source() {
    myq()
        .args(["auth", "login", "--code", "123456", "--stdin"])
        .write_stdin("dummy-password")
        .assert()
        .code(2)
        .stderr(contains("--code finishes"));
    myq()
        .args([
            "auth",
            "login",
            "--code",
            "123456",
            "--email",
            "dummy@example.invalid",
        ])
        .assert()
        .code(2)
        .stderr(contains("--code finishes"));
}
