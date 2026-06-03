use crate::config::get_data_dir;
use anyhow::{Context, Result};
use cmdhub_shared::{
    AciCommandContract, DbAciRecord, CREATE_APPS_FTS_TABLE, CREATE_APPS_TABLE,
    CREATE_ARGUMENTS_TABLE, CREATE_COMMANDS_VEC_TABLE,
};
use rusqlite::Connection;
use std::path::PathBuf;

pub fn resolve_db_path() -> PathBuf {
    get_data_dir().join("cmdhub.db")
}

pub fn open_db() -> Result<Connection> {
    let db_path = resolve_db_path();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create database parent directories")?;
    }

    unsafe {
        type SqliteVecInitFn = unsafe extern "C" fn();
        let init_fn: SqliteVecInitFn = sqlite_vec::sqlite3_vec_init;
        #[allow(clippy::missing_transmute_annotations)]
        let _ = rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(init_fn)));
    }

    let conn = Connection::open(&db_path).context("Failed to open SQLite database file")?;
    let _ = conn.execute("PRAGMA journal_mode = WAL;", []);
    let _ = conn.execute("PRAGMA synchronous = NORMAL;", []);
    Ok(conn)
}

pub fn init_db(conn: &Connection) -> Result<()> {
    conn.execute(CREATE_APPS_TABLE, [])
        .context("Failed to create apps table")?;
    conn.execute(CREATE_ARGUMENTS_TABLE, [])
        .context("Failed to create arguments table")?;
    conn.execute(CREATE_APPS_FTS_TABLE, [])
        .context("Failed to create apps_fts table")?;

    // Commands vector table may fail to create if sqlite-vec is not fully supported or active
    if let Err(e) = conn.execute(CREATE_COMMANDS_VEC_TABLE, []) {
        eprintln!("Warning: Failed to initialize sqlite-vec commands_vec table: {}. Falling back to FTS5 search.", e);
    }
    Ok(())
}

fn preprocess_query(query: &str, use_and: bool) -> String {
    let stop_words: std::collections::HashSet<&str> = [
        "how", "to", "a", "the", "on", "in", "of", "for", "with", "an", "is", "at", "by", "and",
        "or", "from", "my", "your", "our", "me", "us",
    ]
    .iter()
    .cloned()
    .collect();

    let words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| !stop_words.contains(w.as_str()))
        .map(|w| format!("{}*", w))
        .collect();

    if words.is_empty() {
        "*".to_string()
    } else if use_and {
        words.join(" ")
    } else {
        words.join(" OR ")
    }
}

pub fn search_commands(
    conn: &Connection,
    query: &str,
    query_vector: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<AciCommandContract>> {
    let and_query = preprocess_query(query, true);
    let or_query = preprocess_query(query, false);

    let mut and_match = false;
    if and_query != "*" {
        if let Ok(count) = conn.query_row::<u64, _, _>(
            "SELECT count(*) FROM apps_fts WHERE apps_fts MATCH :query",
            rusqlite::named_params! { ":query": &and_query },
            |row| row.get(0),
        ) {
            if count > 0 {
                and_match = true;
            }
        }
    }

    let processed_query = if and_match { and_query } else { or_query };

    // 1. Fast exact-match short-circuiting check (LOWER check for path/name)
    let trimmed_query = query.trim().to_lowercase();
    let mut exact_stmt = conn.prepare(
        "SELECT \
            arg.app_id, \
            app.name, \
            arg.cmd_path, \
            arg.node_type, \
            arg.description, \
            arg.risk_level, \
            arg.example_template, \
            app.install_instructions, \
            arg.docker_image, \
            arg.script_url, \
            arg.source_url \
        FROM arguments arg \
        JOIN apps app ON arg.app_id = app.app_id \
        WHERE LOWER(arg.cmd_path) = :query OR LOWER(app.name) = :query \
        LIMIT :limit_num",
    )?;

    let exact_rows = exact_stmt.query_map(
        rusqlite::named_params! {
            ":query": trimmed_query,
            ":limit_num": limit,
        },
        |row| {
            Ok(DbAciRecord {
                app_id: row.get(0)?,
                name: row.get(1)?,
                cmd_path: row.get(2)?,
                node_type: row.get(3)?,
                description: row.get(4)?,
                risk_level: row.get(5)?,
                example_template: row.get(6)?,
                install_instructions: row.get(7)?,
                docker_image: row.get(8)?,
                script_url: row.get(9)?,
                source_url: row.get(10)?,
            })
        },
    )?;

    let mut exact_results = Vec::new();
    for record in exact_rows.flatten() {
        if let Ok(contract) = AciCommandContract::try_from(record) {
            exact_results.push(contract);
        }
    }

    // Check if commands_vec table exists and has any data
    let mut has_vector_db = false;
    if query_vector.is_some() {
        if let Ok(count) = conn.query_row::<u64, _, _>(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='commands_vec'",
            [],
            |row| row.get(0),
        ) {
            if count > 0 {
                if let Ok(vec_count) =
                    conn.query_row::<u64, _, _>("SELECT count(*) FROM commands_vec", [], |row| {
                        row.get(0)
                    })
                {
                    if vec_count > 0 {
                        has_vector_db = true;
                    }
                }
            }
        }
    }

    if has_vector_db {
        let q_vec = query_vector.unwrap();
        let mut vec_bytes = Vec::with_capacity(q_vec.len() * 4);
        for &val in q_vec {
            vec_bytes.extend_from_slice(&val.to_ne_bytes());
        }
        let mut stmt = conn.prepare(
            "WITH fts_rank AS ( \
                SELECT cmd_path, row_number() OVER (ORDER BY bm25(apps_fts, 0.0, 10.0, 1.0) ASC) as fts_pos \
                FROM apps_fts WHERE apps_fts MATCH :query \
                LIMIT 100 \
            ), \
            vec_rank AS ( \
                SELECT cmd_path, row_number() OVER (ORDER BY distance ASC) as vec_pos \
                FROM commands_vec \
                WHERE embedding MATCH :query_vector AND k = 100 \
            ) \
            SELECT \
                arg.app_id, \
                app.name, \
                arg.cmd_path, \
                arg.node_type, \
                arg.description, \
                arg.risk_level, \
                arg.example_template, \
                app.install_instructions, \
                arg.docker_image, \
                arg.script_url, \
                arg.source_url \
            FROM arguments arg \
            JOIN apps app ON arg.app_id = app.app_id \
            LEFT JOIN fts_rank fts ON arg.cmd_path = fts.cmd_path \
            LEFT JOIN vec_rank vec ON arg.cmd_path = vec.cmd_path \
            WHERE fts.cmd_path IS NOT NULL OR vec.cmd_path IS NOT NULL \
            ORDER BY COALESCE(1.0 / (60.0 + fts.fts_pos), 0.0) + COALESCE(1.0 / (60.0 + vec.vec_pos), 0.0) DESC \
            LIMIT :limit_num"
        )?;

        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":query": processed_query,
                ":query_vector": vec_bytes,
                ":limit_num": limit,
            },
            |row| {
                Ok(DbAciRecord {
                    app_id: row.get(0)?,
                    name: row.get(1)?,
                    cmd_path: row.get(2)?,
                    node_type: row.get(3)?,
                    description: row.get(4)?,
                    risk_level: row.get(5)?,
                    example_template: row.get(6)?,
                    install_instructions: row.get(7)?,
                    docker_image: row.get(8)?,
                    script_url: row.get(9)?,
                    source_url: row.get(10)?,
                })
            },
        )?;

        let mut results = Vec::new();
        for r in rows {
            let record = r?;
            if let Ok(contract) = AciCommandContract::try_from(record) {
                results.push(contract);
            }
        }
        let mut final_results = exact_results.clone();
        final_results.append(&mut results);
        Ok(final_results)
    } else {
        // Fallback to pure FTS5 MATCH BM25 search
        let mut stmt = conn.prepare(
            "SELECT \
                arg.app_id, \
                app.name, \
                arg.cmd_path, \
                arg.node_type, \
                arg.description, \
                arg.risk_level, \
                arg.example_template, \
                app.install_instructions, \
                arg.docker_image, \
                arg.script_url, \
                arg.source_url \
            FROM arguments arg \
            JOIN apps app ON arg.app_id = app.app_id \
            JOIN apps_fts fts ON arg.cmd_path = fts.cmd_path \
            WHERE apps_fts MATCH :query \
            ORDER BY bm25(apps_fts, 0.0, 10.0, 1.0) ASC \
            LIMIT :limit_num",
        )?;

        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":query": processed_query,
                ":limit_num": limit,
            },
            |row| {
                Ok(DbAciRecord {
                    app_id: row.get(0)?,
                    name: row.get(1)?,
                    cmd_path: row.get(2)?,
                    node_type: row.get(3)?,
                    description: row.get(4)?,
                    risk_level: row.get(5)?,
                    example_template: row.get(6)?,
                    install_instructions: row.get(7)?,
                    docker_image: row.get(8)?,
                    script_url: row.get(9)?,
                    source_url: row.get(10)?,
                })
            },
        )?;

        let mut results = Vec::new();
        for r in rows {
            let record = r?;
            if let Ok(contract) = AciCommandContract::try_from(record) {
                results.push(contract);
            }
        }
        let mut final_results = exact_results.clone();
        final_results.append(&mut results);
        Ok(final_results)
    }
}

pub fn search_all(
    conn: &Connection,
    query: &str,
    query_vector: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<AciCommandContract>> {
    let mut results = search_commands(conn, query, query_vector, limit)?;

    let config_dir = crate::config::get_config_dir();
    let skills_dir = config_dir.join("skills");
    let local_skill = cmdhub_skills::LocalFileSkill::new(skills_dir);

    let mut registry = cmdhub_skills::SkillRegistry::new();
    registry.register(Box::new(local_skill));

    if let Ok(mut skill_results) = registry.resolve(query) {
        results.append(&mut skill_results);
    }

    let mut seen = std::collections::HashSet::new();
    results.retain(|item| seen.insert(item.cmd_path.clone()));
    results.truncate(limit);

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match_priority() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO apps (app_id, name, install_instructions) VALUES (?, ?, ?)",
            ("org.test.git", "git", "{}"),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            ("git", "org.test.git", "git", "root", "Git version control", "safe", "git"),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?, ?, ?)",
            ("git", "git", "Git version control"),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO apps (app_id, name, install_instructions) VALUES (?, ?, ?)",
            ("org.test.gitleaks", "gitleaks", "{}"),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            ("gitleaks", "org.test.gitleaks", "gitleaks", "root", "Detect secrets in git", "safe", "gitleaks"),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?, ?, ?)",
            ("gitleaks", "gitleaks", "Detect secrets in git"),
        )
        .unwrap();

        let res = search_commands(&conn, "git", None, 10).unwrap();
        assert!(!res.is_empty());
        assert_eq!(res[0].cmd_path, "git");
    }

    #[test]
    fn test_fts_fallback_and_or() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Setup test apps and arguments
        conn.execute(
            "INSERT INTO apps (app_id, name, install_instructions) VALUES (?, ?, ?)",
            ("org.test.rm", "rm", "{}"),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            ("rm", "org.test.rm", "rm", "root", "delete local files", "safe", "rm"),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?, ?, ?)",
            ("rm", "rm", "delete local files"),
        )
        .unwrap();

        // 1. Search that matches AND exactly: "delete local files"
        let res = search_commands(&conn, "delete local files", None, 10).unwrap();
        assert!(!res.is_empty());
        assert_eq!(res[0].cmd_path, "rm");

        // 2. Search that matches AND with stop words: "delete my local files"
        let res = search_commands(&conn, "delete my local files", None, 10).unwrap();
        assert!(!res.is_empty());
        assert_eq!(res[0].cmd_path, "rm");

        // 3. Search that has no complete AND matches: "delete missing files" (FTS AND query = "delete* AND missing* AND files*")
        // It must fallback to OR and still match "rm" (matches delete, files)
        let res = search_commands(&conn, "delete missing files", None, 10).unwrap();
        assert!(!res.is_empty());
        assert_eq!(res[0].cmd_path, "rm");
    }

    #[test]
    fn test_hybrid_search_knn_match() {
        unsafe {
            type SqliteVecInitFn = unsafe extern "C" fn();
            let init_fn: SqliteVecInitFn = sqlite_vec::sqlite3_vec_init;
            #[allow(clippy::missing_transmute_annotations)]
            let _ = rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(init_fn)));
        }

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO apps (app_id, name, install_instructions) VALUES (?, ?, ?)",
            ("org.test.knn", "knn", "{}"),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            ("knn", "org.test.knn", "knn", "root", "vector search helper", "safe", "knn"),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?, ?, ?)",
            ("knn", "knn", "vector search helper"),
        )
        .unwrap();

        // Insert vector
        let v = vec![0.1f32; 512];
        let mut v_bytes = Vec::with_capacity(512 * 4);
        for &val in &v {
            v_bytes.extend_from_slice(&val.to_ne_bytes());
        }

        conn.execute(
            "INSERT INTO commands_vec (cmd_path, embedding) VALUES (?, ?)",
            ("knn", v_bytes),
        )
        .unwrap();

        // Search with query vector
        let query_vec = vec![0.12f32; 512];
        let res = search_commands(&conn, "missing_term", Some(&query_vec), 10).unwrap();
        assert!(!res.is_empty());
        assert_eq!(res[0].cmd_path, "knn");
    }
}
