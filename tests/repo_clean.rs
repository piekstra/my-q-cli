//! This repository is public. Everything tracked in git stays free of personal
//! data and of credentials. Checks shapes, never a list of real values: such a
//! list would publish exactly what it exists to keep out.

use std::process::Command;

fn tracked_files() -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files"])
        .output()
        .expect("git runs, or this guard checks nothing");
    assert!(out.status.success(), "git ls-files failed");
    let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        !files.is_empty(),
        "git tracks no files, so nothing is checked"
    );
    files
}

fn private_ipv4(text: &str) -> Vec<String> {
    // RFC 5737's documentation ranges are the only addresses a doc may show.
    let mut found = Vec::new();
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
            let private = token.starts_with("10.")
                || token.starts_with("192.168.")
                || (token.starts_with("172.")
                    && (16..32).contains(&parts[1].parse::<u8>().unwrap_or(0)));
            if private && token != "10.0.2.2" {
                found.push(token.to_string());
            }
        }
    }
    found
}

fn hex_macs(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    for window in bytes.windows(17) {
        let shaped = window.iter().enumerate().all(|(i, b)| {
            if i % 3 == 2 {
                *b == b':' || *b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        });
        // 00:00:5E:00:53 is RFC 7042's documentation block.
        if shaped
            && !window.starts_with(b"00-00-5E-00-53")
            && !window.to_ascii_lowercase().starts_with(b"00:00:5e:00:53")
        {
            count += 1;
        }
    }
    count
}

#[test]
fn tracked_files_carry_no_personal_data_or_credentials() {
    let files = tracked_files();
    let mut problems = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        if file == "tests/repo_clean.rs" {
            continue;
        }
        for ip in private_ipv4(&text) {
            problems.push(format!("{file}: home-network address {ip}"));
        }
        if hex_macs(&text) > 0 {
            problems.push(format!("{file}: a MAC address"));
        }
        for shape in ["eyJhbGci", "BEGIN PRIVATE KEY", "BEGIN RSA", "Bearer ey"] {
            if text.contains(shape) {
                problems.push(format!("{file}: a credential shape ({shape})"));
            }
        }
        for word in text.split(|c: char| c.is_whitespace() || "\"'`<>(),;".contains(c)) {
            if let Some((local, domain)) = word.split_once('@') {
                let allowed = domain.ends_with("example.com")
                    || domain.ends_with("example.invalid")
                    || domain.ends_with("users.noreply.github.com");
                let looks_like_address = domain.contains('.')
                    && !local.is_empty()
                    && local
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c));
                if looks_like_address && !allowed {
                    problems.push(format!("{file}: an email address"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn no_raw_capture_or_apk_is_tracked() {
    for file in tracked_files() {
        for suffix in [".apk", ".flow", ".jsonl", ".pem", ".p12", ".dex"] {
            assert!(!file.ends_with(suffix), "{file} must not be tracked");
        }
    }
}
