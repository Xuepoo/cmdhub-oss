use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn test_boundary_empty_query() {
    let tmp = TempDir::new().unwrap();
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", tmp.path())
        .env("CMDH_NO_STARTER", "1")
        .env("XDG_CONFIG_HOME", tmp.path())
        .arg("search")
        .arg("");
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(json.as_array().map(|a| a.is_empty()).unwrap_or(false));
}

#[test]
fn test_boundary_ultra_long_query() {
    let tmp = TempDir::new().unwrap();
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    let ultra_long = "a".repeat(1500);
    cmd.env("XDG_DATA_HOME", tmp.path())
        .env("CMDH_NO_STARTER", "1")
        .env("XDG_CONFIG_HOME", tmp.path())
        .arg("search")
        .arg(ultra_long);
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(json.as_array().map(|a| a.is_empty()).unwrap_or(false));
}

#[test]
fn test_boundary_quotes_semicolons_emoji() {
    let tmp = TempDir::new().unwrap();
    let edge_cases = vec![
        "\"double quotes\"",
        "'single quotes'",
        "'; DROP TABLE apps; -- sql injection test",
        "semicolon; test",
        "emoji 🚀🔥🌟 test",
        "\\\\ backslash \\\\",
    ];
    for query in edge_cases {
        let mut cmd = Command::cargo_bin("cmdh").unwrap();
        cmd.env("XDG_DATA_HOME", tmp.path())
            .env("CMDH_NO_STARTER", "1")
            .env("XDG_CONFIG_HOME", tmp.path())
            .arg("search")
            .arg(query);
        let assert = cmd.assert().success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert!(json.as_array().map(|a| a.is_empty()).unwrap_or(false));
    }
}

#[test]
fn test_boundary_limit_zero() {
    let tmp = TempDir::new().unwrap();
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", tmp.path())
        .env("CMDH_NO_STARTER", "1")
        .env("XDG_CONFIG_HOME", tmp.path())
        .arg("search")
        .arg("test")
        .arg("--limit")
        .arg("0");
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(json.as_array().map(|a| a.is_empty()).unwrap_or(false));
}

#[test]
fn test_boundary_limit_large() {
    let tmp = TempDir::new().unwrap();
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", tmp.path())
        .env("CMDH_NO_STARTER", "1")
        .env("XDG_CONFIG_HOME", tmp.path())
        .arg("search")
        .arg("test")
        .arg("--limit")
        .arg("999999");
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(json.as_array().map(|a| a.is_empty()).unwrap_or(false));
}
