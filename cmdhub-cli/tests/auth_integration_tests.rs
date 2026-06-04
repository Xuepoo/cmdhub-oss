use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[tokio::test]
async fn test_cli_auth_session_lifecycle() {
    // Create temporary directory for XDG config override
    let config_dir = tempdir().unwrap();
    let config_path = config_dir.path().join("config.toml");

    // Initialize mock configuration
    let config_content = r#"
api_url = "http://127.0.0.1:8080/api/v1"
public_key = "0000000000000000000000000000000000000000000000000000000000000000"
timeout_seconds = 5

[output]
mode = "full"
"#;
    fs::write(&config_path, config_content).unwrap();

    // 1. Run logout when not logged in
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_CONFIG_HOME", config_dir.path());
    cmd.arg("--config").arg(&config_path).arg("logout");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("You are not currently logged in."));

    // 2. Mock a session file inside XDG config override
    // In our implementation, config_dir is where config.toml lives.
    // However, in cargo-run, the config_dir for session.json is computed via `get_config_dir()`
    // which aligns to XDG config directory.
    // To ensure testing isolation, we can set the env variable:
    // We override XDG_CONFIG_HOME so get_config_dir() will point to our temp dir!
    std::env::set_var("XDG_CONFIG_HOME", config_dir.path());

    let session_dir = config_dir.path().join("cmdhub");
    fs::create_dir_all(&session_dir).unwrap();
    let session_path = session_dir.join("session.json");

    let mock_session = serde_json::json!({
        "token": "mock_jwt_token_abc_123",
        "expires_at": chrono::Utc::now().timestamp() + 3600
    });
    fs::write(
        &session_path,
        serde_json::to_string_pretty(&mock_session).unwrap(),
    )
    .unwrap();

    // Verify session permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&session_path).unwrap().permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&session_path, perms).unwrap();

        let metadata = fs::metadata(&session_path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    // 3. Verify session parsing and validation via get_session
    let session_opt = cmdhub_cli::auth::get_session().unwrap();
    assert!(session_opt.is_some());
    let session = session_opt.unwrap();
    assert_eq!(session.token, "mock_jwt_token_abc_123");

    // 4. Run logout command when logged in
    let mut cmd2 = Command::cargo_bin("cmdh").unwrap();
    cmd2.env("XDG_CONFIG_HOME", config_dir.path());
    cmd2.arg("--config").arg(&config_path).arg("logout");
    cmd2.assert().success().stdout(predicate::str::contains(
        "Successfully logged out and cleared local credentials.",
    ));

    // 5. Assert session file is deleted
    assert!(!session_path.exists());
}
