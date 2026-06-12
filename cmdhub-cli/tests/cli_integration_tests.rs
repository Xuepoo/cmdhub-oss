use cmdhub_cli::config::{load_or_create_config, resolve_config_path, Config};
use cmdhub_cli::db::{init_db, open_db, search_commands};
use cmdhub_cli::runner::{get_command_by_path, run_command};
use cmdhub_shared::RiskLevel;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use tempfile::TempDir;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

#[test]
fn test_config_resolution() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().to_path_buf();

    // Set XDG_CONFIG_HOME to the temp directory
    std::env::set_var("XDG_CONFIG_HOME", &config_dir);

    // Load or create config (should create it)
    let config = load_or_create_config(None).unwrap();
    assert_eq!(config.api_url, "https://cdn.cmdhub.org");
    assert_eq!(config.timeout_seconds, 30);

    // Verify it exists in config path
    let expected_path = resolve_config_path(None);
    assert!(expected_path.exists());
}

#[test]
fn test_config_env_override() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let env_config_path = tmp.path().join("env_config.toml");

    // Set CMDH_CONFIG env var
    std::env::set_var("CMDH_CONFIG", &env_config_path);

    // Loading should fail because the override file does not exist
    let result = load_or_create_config(None);
    assert!(result.is_err());

    // Create the file
    let default_config = cmdhub_cli::config::Config::default();
    let toml_str = toml::to_string_pretty(&default_config).unwrap();
    std::fs::write(&env_config_path, toml_str).unwrap();

    // Now loading should succeed
    let config = load_or_create_config(None).unwrap();
    assert_eq!(config.api_url, "https://cdn.cmdhub.org");

    // Verify it exists at the exact CMDH_CONFIG path
    let expected_path = resolve_config_path(None);
    assert_eq!(expected_path, env_config_path);
    assert!(expected_path.exists());

    // Clean up env var so it doesn't affect other tests
    std::env::remove_var("CMDH_CONFIG");
}

#[test]
fn test_config_custom_path_override() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let custom_path = tmp.path().join("custom_config.toml");

    // Loading should fail because the custom path does not exist
    let result = load_or_create_config(Some(custom_path.clone()));
    assert!(result.is_err());

    // Create the file
    let default_config = cmdhub_cli::config::Config::default();
    let toml_str = toml::to_string_pretty(&default_config).unwrap();
    std::fs::write(&custom_path, toml_str).unwrap();

    // Now loading with custom path should succeed
    let config = load_or_create_config(Some(custom_path.clone())).unwrap();
    assert_eq!(config.api_url, "https://cdn.cmdhub.org");

    // Verify it exists at the exact custom path
    let expected_path = resolve_config_path(Some(custom_path.clone()));
    assert_eq!(expected_path, custom_path);
    assert!(expected_path.exists());
}

#[test]
fn test_search_fallback_and_db() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Set XDG_DATA_HOME to temp dir
    std::env::set_var("XDG_DATA_HOME", &data_dir);

    let conn = open_db().unwrap();
    init_db(&conn).unwrap();

    // Insert dummy records for app and argument
    conn.execute(
        "INSERT INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
        ("org.github.sl", "sl", "{\"brew\": \"brew install sl\"}"),
    )
    .unwrap();

    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            "sl.-l",
            "org.github.sl",
            "-l",
            "arg",
            "Display a train moving from left to right",
            "safe",
            "sl -l",
        ),
    ).unwrap();

    // Insert into FTS5 virtual table
    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
        ("sl.-l", "sl", "Display a train moving from left to right"),
    )
    .unwrap();

    // Search and verify fallback to pure FTS5 works
    let results = search_commands(&conn, "train", None, 5).unwrap();
    assert_eq!(results.len(), 1);

    let command = &results[0];
    assert_eq!(command.cmd_path, "sl.-l");
    assert_eq!(command.app_id, "org.github.sl");
    assert_eq!(command.name, "sl");
    assert_eq!(command.risk_level, RiskLevel::Safe);
    assert_eq!(command.example_template, Some("sl -l".to_string()));
    assert_eq!(
        command.install_instructions.as_ref().unwrap().brew,
        Some("brew install sl".to_string())
    );
}

#[test]
fn test_safety_gating() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Set XDG_DATA_HOME to temp dir
    std::env::set_var("XDG_DATA_HOME", &data_dir);
    std::env::set_var("CMD_TEST", "1");

    let conn = open_db().unwrap();
    init_db(&conn).unwrap();

    // Insert a dangerous command (we mock it as "echo" for child process testing)
    conn.execute(
        "INSERT INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
        ("org.test.echo", "echo", None::<String>),
    )
    .unwrap();

    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            "echo.danger",
            "org.test.echo",
            "danger",
            "arg",
            "Dangerous echo",
            "dangerous",
            "echo danger",
        ),
    ).unwrap();

    // Retrieve from DB
    let cmd = get_command_by_path(&conn, "echo.danger").unwrap();
    assert_eq!(cmd.risk_level, RiskLevel::Dangerous);

    let config = Config::default();

    // Test bypass gate
    let result = run_command(&config, &conn, "echo.danger", &["hello".to_string()], true);
    assert!(result.is_ok());

    // Test dangerous blocked when skip_gating is false and stdin is not interactive (fails read_line)
    let result = run_command(&config, &conn, "echo.danger", &["hello".to_string()], false);
    assert!(result.is_err());
    let err_str = format!("{}", result.unwrap_err());
    assert!(
        err_str.contains("blocked")
            || err_str.contains("read_line")
            || err_str.contains("standard input")
    );
}

#[test]
fn test_run_command_risk_gating_dangerous_blocked_by_config() {
    let tmp = TempDir::new().unwrap();
    let conn_path = tmp.path().join("test.db");
    let conn = rusqlite::Connection::open(&conn_path).unwrap();
    init_db(&conn).unwrap();

    // Insert a dangerous command ACI
    conn.execute(
        "INSERT OR REPLACE INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
        ("org.test.echo", "echo", None::<String>),
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            "echo.danger",
            "org.test.echo",
            "danger",
            "arg",
            "Dangerous echo",
            "dangerous",
            "echo danger",
        ),
    ).unwrap();

    // Set config risk_guard_level to "block"
    let config = Config {
        risk_guard_level: "block".to_string(),
        ..Config::default()
    };

    // Must be blocked immediately even if we try to run it
    let result = run_command(&config, &conn, "echo.danger", &["hello".to_string()], false);
    assert!(result.is_err());
    let err_str = format!("{}", result.unwrap_err());
    assert!(err_str.contains("blocked"));
}

#[test]
fn test_run_command_risk_gating_dangerous_allowed_by_config() {
    let tmp = TempDir::new().unwrap();
    let conn_path = tmp.path().join("test.db");
    let conn = rusqlite::Connection::open(&conn_path).unwrap();
    init_db(&conn).unwrap();

    // Insert a dangerous command ACI
    conn.execute(
        "INSERT OR REPLACE INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
        ("org.test.echo", "echo", None::<String>),
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            "echo.danger",
            "org.test.echo",
            "danger",
            "arg",
            "Dangerous echo",
            "dangerous",
            "echo danger",
        ),
    ).unwrap();

    // Set config risk_guard_level to "allow"
    let config = Config {
        risk_guard_level: "allow".to_string(),
        ..Config::default()
    };

    // Must execute successfully because it's allowed
    let result = run_command(&config, &conn, "echo.danger", &["hello".to_string()], false);
    assert!(result.is_ok());
}

#[test]
fn test_signature_verification_and_zstd() {
    // Generate deterministic key pair using [42; 32] seed
    let seed = [42u8; 32];
    let signing_key = SigningKey::from_bytes(&seed);
    let verifying_key = signing_key.verifying_key();
    let pub_key_bytes = verifying_key.to_bytes();

    // Dummy DB content
    let db_payload = b"SQLite dummy content";

    // Decompress/compress zstd
    let compressed = zstd::encode_all(&db_payload[..], 3).unwrap();

    // Compute SHA-256
    let mut hasher = Sha256::new();
    hasher.update(&compressed);
    let hash_result: [u8; 32] = hasher.finalize().into();

    // Sign using private key
    let signature = signing_key.sign(&hash_result);
    let sig_bytes = signature.to_bytes();

    // Verify signature using pubkey
    let verifying_key_dec = VerifyingKey::from_bytes(&pub_key_bytes).unwrap();
    let sig_dec = Signature::from_slice(&sig_bytes).unwrap();
    let verify_res = verifying_key_dec.verify(&hash_result, &sig_dec);
    assert!(verify_res.is_ok());

    // Decompress payload
    let decompressed = zstd::decode_all(&compressed[..]).unwrap();
    assert_eq!(decompressed, db_payload);
}

#[test]
fn test_skills_integration() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().to_path_buf();

    // Set XDG_CONFIG_HOME and XDG_DATA_HOME to temp dirs
    std::env::set_var("XDG_CONFIG_HOME", &config_dir);
    std::env::set_var("XDG_DATA_HOME", &config_dir);

    // Load db and init
    let conn = open_db().unwrap();
    init_db(&conn).unwrap();

    // Create a mock skills JSON file inside skills_dir
    let skills_dir = config_dir.join("cmdhub").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();

    let contract_custom = cmdhub_shared::AciCommandContract {
        app_id: "org.test.custom".to_string(),
        name: "custom_cmd".to_string(),
        cmd_path: "custom.run".to_string(),
        node_type: cmdhub_shared::NodeType::Root,
        description: "A completely custom command shortcut loaded from skills".to_string(),
        risk_level: RiskLevel::Safe,
        example_template: Some("custom_cmd --do-something".to_string()),
        os_aliases: None,
        install_instructions: None,
        docker_image: None,
        script_url: None,
        source_url: None,
        popularity: 0.0,
    };

    let json_content = serde_json::to_string(&contract_custom).unwrap();
    std::fs::write(skills_dir.join("custom.json"), json_content).unwrap();

    // Search query using search_all and verify it successfully recalls the skill command!
    let results = search_commands(&conn, "completely", None, 5).unwrap();
    assert!(results.is_empty()); // Should be empty in pure DB search

    let results_all = cmdhub_cli::db::search_all(&conn, "completely", None, 5).unwrap();
    assert_eq!(results_all.len(), 1);
    assert_eq!(results_all[0].name, "custom_cmd");
    assert_eq!(results_all[0].cmd_path, "custom.run");
}

#[test]
fn test_config_override_strict_validation() {
    use assert_cmd::Command;
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.arg("--config")
        .arg("non_existent_config_abc_123.toml")
        .arg("search")
        .arg("test");
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("does not exist"));
}

#[test]
fn test_output_preset_formatting() {
    let _guard = ENV_MUTEX.lock().unwrap();
    use assert_cmd::Command;
    let tmp = tempfile::TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Set XDG_DATA_HOME to temp dir
    std::env::set_var("XDG_DATA_HOME", &data_dir);

    let conn = open_db().unwrap();
    init_db(&conn).unwrap();

    conn.execute(
        "INSERT INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
        ("org.github.git", "git", "{\"brew\": \"brew install git\"}"),
    )
    .unwrap();

    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            "git",
            "org.github.git",
            "git",
            "root",
            "git version control",
            "safe",
            "example_template",
        ),
    ).unwrap();

    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
        ("git", "git", "git version control"),
    )
    .unwrap();

    drop(conn);

    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", &data_dir)
        .arg("search")
        .arg("git")
        .arg("--usage-only");
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(output.contains("cmd_path"));
    assert!(output.contains("example_template"));
    assert!(!output.contains("risk_level"));
}

#[test]
fn test_init_command_safety_guards() {
    use assert_cmd::Command;
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("cmdhub/config.toml");

    // Seed file
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "dummy").unwrap();

    // Test guard warning exits gracefully (exit code 0)
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_CONFIG_HOME", tmp.path()).arg("init");
    cmd.assert().success();

    let val = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(val, "dummy"); // Should not have changed

    // Overwrite with force
    let mut cmd_force = Command::cargo_bin("cmdh").unwrap();
    cmd_force
        .env("XDG_CONFIG_HOME", tmp.path())
        .arg("init")
        .arg("--force");
    cmd_force.assert().success();

    let val_overwritten = std::fs::read_to_string(&config_path).unwrap();
    assert!(val_overwritten.contains("CmdHub configuration file"));
}

#[test]
fn test_completions_generation() {
    use assert_cmd::Command;
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.arg("completions").arg("zsh");
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("compdef") || stdout.contains("#defzsh"));
}

#[test]
fn test_expanded_aci_fields_roundtrip() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();
    std::env::set_var("XDG_DATA_HOME", &data_dir);

    let conn = open_db().unwrap();
    init_db(&conn).unwrap();

    conn.execute(
        "INSERT INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
        (
            "org.test.extended",
            "ext",
            "{\"brew\": \"brew install ext\", \"scoop\": \"scoop install ext\"}",
        ),
    )
    .unwrap();

    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template, docker_image, script_url, source_url) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        (
            "ext",
            "org.test.extended",
            "ext",
            "root",
            "extended commands",
            "safe",
            "ext --test",
            Some("docker.io/test/ext:latest"),
            Some("https://raw.githubusercontent.com/test/ext/main/install.sh"),
            Some("https://github.com/test/ext"),
        ),
    ).unwrap();

    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
        ("ext", "ext", "extended commands"),
    )
    .unwrap();

    drop(conn);

    use assert_cmd::Command;
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", &data_dir)
        .arg("search")
        .arg("ext")
        .arg("--full");

    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("\"docker_image\":\"docker.io/test/ext:latest\""));
    assert!(stdout
        .contains("\"script_url\":\"https://raw.githubusercontent.com/test/ext/main/install.sh\""));
    assert!(stdout.contains("\"source_url\":\"https://github.com/test/ext\""));
}

#[test]
fn test_windows_scoop_install_suggestions() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();
    std::env::set_var("XDG_DATA_HOME", &data_dir);
    std::env::set_var("XDG_CONFIG_HOME", &data_dir);

    // Write a config overrides stating OS is windows
    let config_path = data_dir.join("cmdhub/config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();

    let mut config = cmdhub_cli::config::Config::default();
    config.install.os = Some("windows".to_string());
    config.install.package_managers = vec!["scoop".to_string(), "cargo".to_string()];
    let toml_str = toml::to_string_pretty(&config).unwrap();
    std::fs::write(&config_path, toml_str).unwrap();

    let conn = open_db().unwrap();
    init_db(&conn).unwrap();

    conn.execute(
        "INSERT INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
        (
            "org.test.win",
            "win",
            "{\"scoop\": \"scoop install win\", \"brew\": \"brew install win\"}",
        ),
    )
    .unwrap();

    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template, docker_image, script_url, source_url) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        (
            "win",
            "org.test.win",
            "win",
            "root",
            "windows commands",
            "safe",
            "win --test",
            None::<String>,
            None::<String>,
            None::<String>,
        ),
    ).unwrap();

    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
        ("win", "win", "windows commands"),
    )
    .unwrap();

    drop(conn);

    use assert_cmd::Command;
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("search")
        .arg("win")
        .arg("--full");

    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("\"install_command\":\"scoop install win\""));
}

#[tokio::test]
async fn test_auto_model_download() {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let cache_dir = tmp.path().to_path_buf();

    // Prepare mock data
    let mock_data = b"mock onnx model content".to_vec();
    let mut hasher = Sha256::new();
    hasher.update(&mock_data);
    let mock_sha256 = format!("{:x}", hasher.finalize());

    // Start ephemeral mock server
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_url = format!("http://127.0.0.1:{}", port);

    let mock_data_clone = mock_data.clone();
    let _server_thread = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer);

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                mock_data_clone.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&mock_data_clone);
            let _ = stream.flush();
        }
    });

    // Set configuration variables
    let config_path = cache_dir.join("cmdhub/config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();

    let temp_model_path = cache_dir.join("cmdhub/models/bge-micro-v2.onnx");

    let mut config = cmdhub_cli::config::Config::default();
    config.vector.model_url = Some(server_url);
    config.vector.model_sha256 = Some(mock_sha256);
    config.vector.model_path = Some(temp_model_path.to_string_lossy().to_string());

    drop(_guard);

    // Trigger ensure_model_installed
    let path = cmdhub_cli::installer::ensure_model_installed(&config)
        .await
        .unwrap();
    assert_eq!(path, temp_model_path);
    assert!(temp_model_path.exists());

    let downloaded_content = std::fs::read(&temp_model_path).unwrap();
    assert_eq!(downloaded_content, mock_data);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_client_incremental_sync_deletions_and_updates() {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().to_path_buf();

    // Set XDG dirs
    std::env::set_var("XDG_CONFIG_HOME", &config_dir);
    std::env::set_var("XDG_DATA_HOME", &config_dir);

    // 1. Initialize DB with some apps/arguments/vecs
    {
        let conn = open_db().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO apps (app_id, name) VALUES ('org.test.app1', 'App One')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO apps (app_id, name) VALUES ('org.test.app2', 'App Two')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level) \
             VALUES ('app1.cmd1', 'org.test.app1', 'cmd1', 'root', 'Test Command 1', 'safe')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level) \
             VALUES ('app2.cmd2', 'org.test.app2', 'cmd2', 'root', 'Test Command 2', 'safe')",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES ('app1.cmd1', 'App One', 'Test Command 1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES ('app2.cmd2', 'App Two', 'Test Command 2')",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO sync_meta (key, value) VALUES ('last_sync_time', '1000')",
            [],
        )
        .unwrap();
    }

    // 2. We mock an IncrementalSyncPayload
    let payload = cmdhub_shared::IncrementalSyncPayload {
        apps: vec![cmdhub_shared::DbApp {
            app_id: "org.test.app2".to_string(),
            name: "App Two Updated".to_string(),
            os_aliases: None,
            install_instructions: None,
            popularity: 0.0,
        }],
        arguments: vec![cmdhub_shared::DbArgument {
            cmd_path: "app2.cmd3".to_string(),
            app_id: "org.test.app2".to_string(),
            node_name: "cmd3".to_string(),
            node_type: "root".to_string(),
            description: "Test Command 3".to_string(),
            risk_level: "safe".to_string(),
            example_template: None,
            docker_image: None,
            script_url: None,
            source_url: None,
        }],
        command_vecs: vec![],
        deleted_apps: vec!["org.test.app1".to_string()],
    };

    // Serialize payload to JSON and compress with zstd
    let json_bytes = serde_json::to_vec(&payload).unwrap();
    let compressed = zstd::encode_all(&json_bytes[..], 3).unwrap();

    // Generate keys to sign
    let seed = [42u8; 32];
    let signing_key = SigningKey::from_bytes(&seed);
    let verifying_key = signing_key.verifying_key();
    let pub_key_bytes = verifying_key.to_bytes();
    let pub_key_hex = pub_key_bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    let mut hasher = Sha256::new();
    hasher.update(&compressed);
    let hash_result: [u8; 32] = hasher.finalize().into();
    let signature = signing_key.sign(&hash_result);
    let sig_bytes = signature.to_bytes().to_vec();

    // Start ephemeral mock server
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_url = format!("http://127.0.0.1:{}", port);

    let manifest = cmdhub_shared::UpdateManifest {
        version: "0.1.0".to_string(),
        mode: Some("incremental".to_string()),
        etag: "mock-etag".to_string(),
        db_url: format!("{}/dummy.db.zst", server_url),
        sig_url: format!("{}/dummy.sig", server_url),
        sha256: hash_result
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>(),
        new_sync_time: Some(2000),
    };

    let manifest_json = serde_json::to_string(&manifest).unwrap();
    let compressed_clone = compressed.clone();
    let sig_bytes_clone = sig_bytes.clone();

    let _server_thread = thread::spawn(move || {
        // 1. Manifest request
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                manifest_json.len(), manifest_json
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
        // 2. Payload request
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                compressed_clone.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&compressed_clone);
            let _ = stream.flush();
        }
        // 3. Signature request
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                sig_bytes_clone.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&sig_bytes_clone);
            let _ = stream.flush();
        }
    });

    let config = cmdhub_cli::config::Config {
        api_url: server_url,
        public_key: pub_key_hex,
        timeout_seconds: 5,
        ..Default::default()
    };

    // Run updater — keep _guard alive through the entire test so that
    // XDG_DATA_HOME stays stable while update_database and open_db run.
    cmdhub_cli::updater::update_database(&config, false)
        .await
        .unwrap();

    // Verify DB state
    let conn = open_db().unwrap();

    // 'org.test.app1' should be deleted (and its commands cascading deleted)
    let app1_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM apps WHERE app_id = 'org.test.app1')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!app1_exists);

    let arg1_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM arguments WHERE cmd_path = 'app1.cmd1')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!arg1_exists);

    // FTS entry for 'app1.cmd1' should be deleted
    let fts1_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM apps_fts WHERE cmd_path = 'app1.cmd1')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!fts1_exists);

    let all_apps: Vec<(String, String)> = conn
        .prepare("SELECT app_id, name FROM apps")
        .unwrap()
        .query_map([], |row| Ok((row.get(0).unwrap(), row.get(1).unwrap())))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    println!("DEBUG: All apps in DB: {:?}", all_apps);

    // 'org.test.app2' name should be updated
    let app2_name: String = conn
        .query_row(
            "SELECT name FROM apps WHERE app_id = 'org.test.app2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(app2_name, "App Two Updated");

    // 'app2.cmd2' should be removed since app2 was updated and cmd2 was not in the payload
    let arg2_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM arguments WHERE cmd_path = 'app2.cmd2')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!arg2_exists);

    // 'app2.cmd3' should exist
    let arg3_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM arguments WHERE cmd_path = 'app2.cmd3')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(arg3_exists);

    // sync_meta should have last_sync_time = '2000'
    let sync_time: String = conn
        .query_row(
            "SELECT value FROM sync_meta WHERE key = 'last_sync_time'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sync_time, "2000");
}
