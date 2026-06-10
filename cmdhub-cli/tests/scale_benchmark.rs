use cmdhub_cli::db::{init_db, open_db, search_commands};
use rusqlite::Connection;
use std::time::Instant;
use tempfile::TempDir;

fn setup_benchmark_db(data_dir: &std::path::Path) -> Connection {
    // 3. Make sure to call sqlite_vec::sqlite3_vec_init via sqlite3_auto_extension BEFORE opening connection
    unsafe {
        type SqliteVecInitFn = unsafe extern "C" fn();
        let init_fn: SqliteVecInitFn = sqlite_vec::sqlite3_vec_init;
        #[allow(clippy::missing_transmute_annotations)]
        let _ = rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(init_fn)));
    }

    std::env::set_var("XDG_DATA_HOME", data_dir);
    let mut conn = open_db().unwrap();

    // Disable synchronous writing constraints to drastically speed up seeding
    conn.execute_batch("PRAGMA synchronous = OFF; PRAGMA journal_mode = MEMORY;")
        .unwrap();

    init_db(&conn).unwrap();

    let tx = conn.transaction().unwrap();

    // 1. Seed 10,000 mock apps
    {
        let mut app_stmt = tx
            .prepare("INSERT INTO apps (app_id, name) VALUES (?1, ?2)")
            .unwrap();
        for i in 1..=10000 {
            app_stmt
                .execute(rusqlite::params![
                    format!("org.mock.app{}", i),
                    format!("App{}", i)
                ])
                .unwrap();
        }
    }

    // 2. Seed 50,000 mock ACI commands, FTS, and vector embeddings
    {
        let mut arg_stmt = tx.prepare(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        ).unwrap();

        let mut fts_stmt = tx
            .prepare("INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)")
            .unwrap();

        let mut vec_stmt = tx
            .prepare("INSERT INTO commands_vec (cmd_path, embedding) VALUES (?1, ?2)")
            .unwrap();

        // 512-dimension dummy vector bytes
        let dummy_vec = vec![0.1f32; 512];
        let mut vec_bytes = Vec::with_capacity(512 * 4);
        for &val in &dummy_vec {
            vec_bytes.extend_from_slice(&val.to_ne_bytes());
        }

        for i in 1..=50000 {
            let app_idx = (i % 10000) + 1;
            let cmd_path = format!("app{}.cmd{}", app_idx, i);
            let app_id = format!("org.mock.app{}", app_idx);
            let node_name = format!("cmd{}", i);
            let description = format!("mock command number {} with capability details", i);

            arg_stmt
                .execute(rusqlite::params![
                    cmd_path,
                    app_id,
                    node_name,
                    "root",
                    description,
                    "safe"
                ])
                .unwrap();

            fts_stmt
                .execute(rusqlite::params![
                    cmd_path,
                    format!("App{}", app_idx),
                    description
                ])
                .unwrap();

            vec_stmt
                .execute(rusqlite::params![cmd_path, &vec_bytes])
                .unwrap();
        }
    }

    tx.commit().unwrap();
    conn
}

#[test]
fn test_hybrid_search_scale_performance() {
    let tmp = TempDir::new().unwrap();
    let conn = setup_benchmark_db(tmp.path());

    // Prepare query vector (512 dimensions)
    let query_vector = vec![0.1f32; 512];

    // Warm up the database and FTS indexes
    let _ = search_commands(&conn, "capability details", Some(&query_vector), 10).unwrap();

    // Benchmark over 20 iterations to compute average latency
    let iterations = 20;
    let start = Instant::now();

    for _ in 0..iterations {
        let results =
            search_commands(&conn, "capability details", Some(&query_vector), 10).unwrap();
        assert!(!results.is_empty());
    }

    let duration = start.elapsed();
    let avg_latency = duration / iterations;

    println!(
        "Benchmark completed: 50,000 records search latency = {:?}",
        avg_latency
    );

    // Performance gate: strict only when CMDH_BENCH_STRICT=1.
    // Default: report-only to prevent spurious failures in debug/CI environments.
    // To enforce: CMDH_BENCH_STRICT=1 cargo test --release --test scale_benchmark
    let strict_mode = std::env::var("CMDH_BENCH_STRICT").as_deref() == Ok("1");
    let threshold_ms: u128 = 50;

    println!(
        "Scale benchmark: avg={:?} strict_mode={}",
        avg_latency, strict_mode
    );

    if strict_mode {
        assert!(
            avg_latency.as_millis() < threshold_ms,
            "Hybrid search latency was too high: {:?} (threshold: {}ms). \
             Only enforced when CMDH_BENCH_STRICT=1.",
            avg_latency,
            threshold_ms
        );
    }
}
