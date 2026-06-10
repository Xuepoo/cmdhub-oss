use assert_cmd::Command;
use cmdhub_cli::db::{init_db, open_db};
use ed25519_dalek::{Signer, SigningKey};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::thread;
use tempfile::TempDir;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

// Setup deterministic key-pair for testing (matches OFFICIAL_PUBLIC_KEY)
fn get_test_keys() -> (SigningKey, String) {
    let seed = [42u8; 32];
    let signing_key = SigningKey::from_bytes(&seed);
    let pub_key_bytes = signing_key.verifying_key().to_bytes();
    let pub_key_hex = pub_key_bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();
    (signing_key, pub_key_hex)
}

fn create_valid_db_zst() -> (Vec<u8>, Vec<u8>, String) {
    let (signing_key, _) = get_test_keys();
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("latest.db");

    // Create and seed a valid staging database
    let conn = Connection::open(&db_path).unwrap();
    init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO apps (app_id, name) VALUES ('org.test.recovery', 'RecoveryApp')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level) \
         VALUES ('recovery.cmd', 'org.test.recovery', 'cmd', 'root', 'recovered test command', 'safe')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES ('recovery.cmd', 'RecoveryApp', 'recovered test command')",
        [],
    ).unwrap();
    drop(conn);

    let db_bytes = fs::read(&db_path).unwrap();
    let compressed = zstd::encode_all(&db_bytes[..], 3).unwrap();

    let mut hasher = Sha256::new();
    hasher.update(&compressed);
    let hash_result: [u8; 32] = hasher.finalize().into();
    let signature = signing_key.sign(&hash_result);
    let sig_bytes = signature.to_bytes().to_vec();
    let sha256_hex = hash_result
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    (compressed, sig_bytes, sha256_hex)
}

#[test]
fn test_recovery_from_db_corruption() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // 1. Write garbage bytes to database to corrupt it
    let db_path = data_dir.join("cmdhub/cmdhub.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    fs::write(
        &db_path,
        b"GARBAGE SQLITE BYTES THAT WILL CORRUPT THE DATABASE FILE!!!",
    )
    .unwrap();

    // 2. Running search should fail cleanly (zero panic, but exit failure)
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("search")
        .arg("recovery");
    let assert = cmd.assert().failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        !stderr.contains("panicked"),
        "Search command panicked on corrupted DB!"
    );

    // 3. Start mock update server to return a fresh valid database
    let (db_zst, sig_bytes, sha256_hex) = create_valid_db_zst();
    let (_, pub_key_hex) = get_test_keys();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_url = format!("http://127.0.0.1:{}", port);

    let manifest = cmdhub_shared::UpdateManifest {
        version: "1.0.0".to_string(),
        mode: Some("full".to_string()),
        etag: "recovery-etag".to_string(),
        db_url: format!("{}/db.zst", server_url),
        sig_url: format!("{}/db.sig", server_url),
        sha256: sha256_hex,
        new_sync_time: Some(3000),
    };
    let manifest_json = serde_json::to_string(&manifest).unwrap();

    let _server_thread = thread::spawn(move || {
        // Manifest request
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                manifest_json.len(), manifest_json
            );
            let _ = stream.write_all(resp.as_bytes());
        }
        // DB Payload request
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                db_zst.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&db_zst);
        }
        // Signature request
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                sig_bytes.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&sig_bytes);
        }
    });

    // Write config pointing to mock server and using test pub key
    let config_path = data_dir.join("cmdhub/config.toml");
    let config_content = format!(
        "api_url = \"{}\"\npublic_key = \"{}\"\ntimeout_seconds = 5\n",
        server_url, pub_key_hex
    );
    fs::write(&config_path, config_content).unwrap();

    // 4. Update the database forcefully
    let mut update_cmd = Command::cargo_bin("cmdh").unwrap();
    update_cmd
        .env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("update")
        .arg("--force");
    update_cmd.assert().success();

    // 5. Search again - should now successfully recover and return the command
    let mut search_cmd = Command::cargo_bin("cmdh").unwrap();
    search_cmd
        .env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("search")
        .arg("recovered");
    let assert = search_cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["cmd_path"], "recovery.cmd");
}

#[test]
fn test_update_invalid_signature_gating() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Setup an initial valid DB
    std::env::set_var("XDG_DATA_HOME", &data_dir);
    let conn = open_db().unwrap();
    init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO apps (app_id, name) VALUES ('org.test.initial', 'InitialApp')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level) \
         VALUES ('initial.cmd', 'org.test.initial', 'cmd', 'root', 'initial command', 'safe')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES ('initial.cmd', 'InitialApp', 'initial command')",
        [],
    ).unwrap();
    drop(conn);

    let (db_zst, _, sha256_hex) = create_valid_db_zst();
    let (_, pub_key_hex) = get_test_keys();
    let invalid_sig = vec![0u8; 64]; // dummy signature that is invalid

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_url = format!("http://127.0.0.1:{}", port);

    let manifest = cmdhub_shared::UpdateManifest {
        version: "1.0.0".to_string(),
        mode: Some("full".to_string()),
        etag: "etag-invalid-sig".to_string(),
        db_url: format!("{}/db.zst", server_url),
        sig_url: format!("{}/db.sig", server_url),
        sha256: sha256_hex,
        new_sync_time: Some(3000),
    };
    let manifest_json = serde_json::to_string(&manifest).unwrap();

    let _server_thread = thread::spawn(move || {
        // Manifest request
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                manifest_json.len(), manifest_json
            );
            let _ = stream.write_all(resp.as_bytes());
        }
        // DB Payload request
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                db_zst.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&db_zst);
        }
        // Signature request (return invalid sig)
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                invalid_sig.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&invalid_sig);
        }
    });

    let config_path = data_dir.join("cmdhub/config.toml");
    let config_content = format!(
        "api_url = \"{}\"\npublic_key = \"{}\"\ntimeout_seconds = 5\n",
        server_url, pub_key_hex
    );
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(&config_path, config_content).unwrap();

    // Try update - should fail due to signature verification
    let mut update_cmd = Command::cargo_bin("cmdh").unwrap();
    update_cmd
        .env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("update")
        .arg("--force");
    update_cmd.assert().failure();

    // Verify database remains untouched (still has initial.cmd and NOT recovery.cmd)
    let mut search_cmd = Command::cargo_bin("cmdh").unwrap();
    search_cmd
        .env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("search")
        .arg("initial");
    let assert = search_cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json.as_array().unwrap()[0]["cmd_path"], "initial.cmd");
}

#[test]
fn test_update_incremental_mid_download_rollback() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // 1. Setup initial valid DB
    std::env::set_var("XDG_DATA_HOME", &data_dir);
    let conn = open_db().unwrap();
    init_db(&conn).unwrap();
    conn.execute(
        "INSERT INTO apps (app_id, name) VALUES ('org.test.initial', 'InitialApp')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level) \
         VALUES ('initial.cmd', 'org.test.initial', 'cmd', 'root', 'initial command', 'safe')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES ('initial.cmd', 'InitialApp', 'initial command')",
        [],
    ).unwrap();
    // last sync time is 1000
    conn.execute(
        "INSERT INTO sync_meta (key, value) VALUES ('last_sync_time', '1000')",
        [],
    )
    .unwrap();
    drop(conn);

    // Prepare an incremental payload zst bytes
    let payload = cmdhub_shared::IncrementalSyncPayload {
        apps: vec![],
        arguments: vec![cmdhub_shared::DbArgument {
            cmd_path: "new.cmd".to_string(),
            app_id: "org.test.initial".to_string(),
            node_name: "new".to_string(),
            node_type: "root".to_string(),
            description: "new command".to_string(),
            risk_level: "safe".to_string(),
            example_template: None,
            docker_image: None,
            script_url: None,
            source_url: None,
        }],
        command_vecs: vec![],
        deleted_apps: vec![],
    };
    let json_bytes = serde_json::to_vec(&payload).unwrap();
    let compressed = zstd::encode_all(&json_bytes[..], 3).unwrap();

    let (signing_key, pub_key_hex) = get_test_keys();
    let mut hasher = Sha256::new();
    hasher.update(&compressed);
    let hash_result: [u8; 32] = hasher.finalize().into();
    let signature = signing_key.sign(&hash_result);
    let sig_bytes = signature.to_bytes().to_vec();
    let sha256_hex = hash_result
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    // Start server that aborts mid-payload download
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_url = format!("http://127.0.0.1:{}", port);

    let manifest = cmdhub_shared::UpdateManifest {
        version: "1.0.1".to_string(),
        mode: Some("incremental".to_string()),
        etag: "etag-incremental-kill".to_string(),
        db_url: format!("{}/incremental.db.zst", server_url),
        sig_url: format!("{}/incremental.sig", server_url),
        sha256: sha256_hex,
        new_sync_time: Some(2000),
    };
    let manifest_json = serde_json::to_string(&manifest).unwrap();

    let _server_thread = thread::spawn(move || {
        // Manifest request
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                manifest_json.len(), manifest_json
            );
            let _ = stream.write_all(resp.as_bytes());
        }
        // DB Payload request - SEND ONLY A FEW BYTES THEN CLOSE STREAM
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                compressed.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            if compressed.len() > 5 {
                let _ = stream.write_all(&compressed[..5]); // partial data
            }
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });

    let config_path = data_dir.join("cmdhub/config.toml");
    let config_content = format!(
        "api_url = \"{}\"\npublic_key = \"{}\"\ntimeout_seconds = 5\n",
        server_url, pub_key_hex
    );
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(&config_path, config_content).unwrap();

    // Trigger update
    let mut update_cmd = Command::cargo_bin("cmdh").unwrap();
    update_cmd
        .env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("update");
    update_cmd.assert().failure();

    // Verify DB was rolled back/untouched and remains fully operational
    let mut search_cmd = Command::cargo_bin("cmdh").unwrap();
    search_cmd
        .env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("search")
        .arg("initial");
    let assert = search_cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json.as_array().unwrap()[0]["cmd_path"], "initial.cmd");

    // Also assert that the new command 'new.cmd' was NOT applied
    let db_path = data_dir.join("cmdhub/cmdhub.db");
    let conn = Connection::open(&db_path).unwrap();
    let new_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM arguments WHERE cmd_path = 'new.cmd')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!new_exists);
}
