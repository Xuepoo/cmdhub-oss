use assert_cmd::Command;
use cmdhub_cli::db::{init_db, open_db};
use rusqlite::Connection;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

// Explicitly ensure the sqlite-vec extension is initialized if needed
fn setup_test_db(data_dir: &std::path::Path) -> Connection {
    unsafe {
        type SqliteVecInitFn = unsafe extern "C" fn();
        let init_fn: SqliteVecInitFn = sqlite_vec::sqlite3_vec_init;
        #[allow(clippy::missing_transmute_annotations)]
        let _ = rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(init_fn)));
    }

    std::env::set_var("XDG_DATA_HOME", data_dir);
    let conn = open_db().unwrap();
    init_db(&conn).unwrap();

    // Insert original records
    conn.execute(
        "INSERT INTO apps (app_id, name, install_instructions) VALUES ('org.test.concurrency', 'ConcurrencyApp', '{}')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level) \
         VALUES ('concurrency.cmd', 'org.test.concurrency', 'cmd', 'root', 'concurrency test command', 'safe')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES ('concurrency.cmd', 'ConcurrencyApp', 'concurrency test command')",
        [],
    ).unwrap();

    conn
}

#[test]
fn test_concurrency_wal_mode_reader_safety() {
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Setup DB and insert initial records
    let _conn = setup_test_db(&data_dir);

    // Keep data_dir path string for threads
    let data_dir_clone = Arc::new(data_dir.clone());
    let data_dir_for_writer = Arc::clone(&data_dir_clone);

    // Start a thread that will acquire a write lock/transaction and sleep
    let writer_handle = thread::spawn(move || {
        // Open a separate connection for writing
        let db_path = data_dir_for_writer.join("cmdhub").join("cmdhub.db");
        let mut conn = Connection::open(&db_path).unwrap();
        // PRAGMA journal_mode returns a result row, use execute_batch to avoid ExecuteReturnedResults
        conn.execute_batch("PRAGMA journal_mode = WAL;").unwrap();

        // Begin transaction
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();

        // Insert new records inside transaction (install_instructions required)
        tx.execute(
            "INSERT INTO apps (app_id, name, install_instructions) VALUES ('org.test.tx', 'TxApp', '{}')",
            [],
        )
        .unwrap();

        // Sleep to hold the transaction open
        thread::sleep(Duration::from_millis(1000));

        tx.commit().unwrap();
    });

    // Let the writer thread start and acquire the lock
    thread::sleep(Duration::from_millis(200));

    // Spawn 10 concurrent readers using the CLI tool in parallel
    let mut reader_handles = vec![];
    for _ in 0..10 {
        let dir = Arc::clone(&data_dir_clone);
        let handle = thread::spawn(move || {
            let mut cmd = Command::cargo_bin("cmdh").unwrap();
            cmd.env("XDG_DATA_HOME", &*dir)
                .env("XDG_CONFIG_HOME", &*dir)
                .arg("search")
                .arg("concurrency");
            let assert = cmd.assert().success();
            let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
            let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

            // Should successfully find the concurrency command
            let arr = json.as_array().unwrap();
            assert!(!arr.is_empty(), "Reader should have returned results");
            assert_eq!(arr[0]["cmd_path"], "concurrency.cmd");
        });
        reader_handles.push(handle);
    }

    // Wait for all readers to complete
    for handle in reader_handles {
        handle.join().unwrap();
    }

    // Wait for the writer to complete and commit
    writer_handle.join().unwrap();

    // Now verify the database is updated and readers can see the newly committed App
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("search")
        .arg("TxApp");
    let assert = cmd.assert().success();
    let _stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    // Since TxApp has no ACI command, it might not show in search_commands unless it matches command name.
    // Let's directly query the DB or insert a command for TxApp to verify.
    let db_path = data_dir.join("cmdhub").join("cmdhub.db");
    let conn = Connection::open(&db_path).unwrap();
    let app_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM apps WHERE app_id = 'org.test.tx')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        app_exists,
        "New app should have been successfully committed"
    );
}
