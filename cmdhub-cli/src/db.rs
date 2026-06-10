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
    let _ = conn.execute("PRAGMA foreign_keys = ON;", []);
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

    conn.execute(
        "CREATE TABLE IF NOT EXISTS sync_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
        [],
    )
    .context("Failed to create sync_meta table")?;

    Ok(())
}

/// Domain concept → concrete terms the tools actually use. Bridges abstract
/// natural-language queries to real command names (cloud CLIs say "vpc"/"subnet",
/// not "networking"). Only applied to the OR-fallback query, so it never breaks
/// precise AND matches — it just widens recall for vague intent queries.
fn concept_synonyms(token: &str) -> &'static [&'static str] {
    match token {
        "networking" | "network" => &["vpc", "subnet", "gateway", "route", "firewall"],
        "firewall" => &["security", "firewall", "acl"],
        "storage" => &["bucket", "volume", "disk", "blob"],
        "database" | "db" => &["database", "sql", "table", "rds"],
        "serverless" => &["lambda", "function", "faas"],
        "container" | "containers" => &["container", "image", "pod"],
        "kubernetes" | "k8s" => &["pod", "deployment", "namespace", "cluster"],
        "secret" | "secrets" => &["secret", "credential", "key", "vault"],
        "dns" => &["dns", "domain", "record", "zone"],
        _ => &[],
    }
}

fn preprocess_query(query: &str, use_and: bool) -> String {
    let stop_words: std::collections::HashSet<&str> = [
        "how", "to", "a", "the", "on", "in", "of", "for", "with", "an", "is", "at", "by", "and",
        "or", "from", "my", "your", "our", "me", "us",
    ]
    .iter()
    .cloned()
    .collect();

    let base: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| !stop_words.contains(w.as_str()))
        .collect();

    let mut terms: Vec<String> = base.iter().map(|w| format!("{}*", w)).collect();

    // OR-query only: widen with domain synonyms so e.g. "configure networking" also
    // matches vpc/subnet/gateway commands. AND-query stays strict (exact intent).
    if !use_and {
        let mut seen: std::collections::HashSet<String> = base.iter().cloned().collect();
        for w in &base {
            for syn in concept_synonyms(w) {
                if seen.insert((*syn).to_string()) {
                    terms.push(format!("{}*", syn));
                }
            }
        }
    }

    if terms.is_empty() {
        "*".to_string()
    } else if use_and {
        terms.join(" ")
    } else {
        terms.join(" OR ")
    }
}

pub fn search_cascading(
    conn: &Connection,
    query: &str,
    query_vector: Option<&[f32]>,
    limit: usize,
    enable_vector: bool,
) -> Result<Vec<AciCommandContract>> {
    let and_query = preprocess_query(query, true);
    let or_query = preprocess_query(query, false);

    // Compute vec_bytes once; reused for all KNN queries below.
    let vec_bytes: Option<Vec<u8>> = if enable_vector {
        query_vector.map(|q_vec| {
            let mut bytes = Vec::with_capacity(q_vec.len() * 4);
            for &val in q_vec {
                bytes.extend_from_slice(&val.to_le_bytes());
            }
            bytes
        })
    } else {
        None
    };

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

    // Whether the OR-query matches any FTS doc. A verbose natural-language query
    // ("I want to know how to configure networking using AWS") rarely AND-matches and
    // its embedding can sit just past the vector threshold — but it still keyword-matches
    // ("aws", "networking"). We must not bail out to empty results in that case.
    let mut or_match = false;
    if or_query != "*" {
        if let Ok(count) = conn.query_row::<u64, _, _>(
            "SELECT count(*) FROM apps_fts WHERE apps_fts MATCH :query",
            rusqlite::named_params! { ":query": &or_query },
            |row| row.get(0),
        ) {
            or_match = count > 0;
        }
    }

    let processed_query = if and_match {
        and_query.clone()
    } else {
        or_query.clone()
    };

    // 1. Fast exact-match check (LOWER check for path/name)
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
            app.os_aliases, \
            app.install_instructions, \
            arg.docker_image, \
            arg.script_url, \
            arg.source_url \
        FROM arguments arg \
        JOIN apps app ON arg.app_id = app.app_id \
        WHERE LOWER(arg.cmd_path) = :query \
           OR (LOWER(app.name) = :query AND arg.node_type = 'root') \
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
                os_aliases: row.get(7)?,
                install_instructions: row.get(8)?,
                docker_image: row.get(9)?,
                script_url: row.get(10)?,
                source_url: row.get(11)?,
            })
        },
    )?;

    let mut exact_results = Vec::new();
    for record in exact_rows.flatten() {
        if let Ok(contract) = AciCommandContract::try_from(record) {
            exact_results.push(contract);
        }
    }

    // 2. Stage 1 App Filter: Threshold check
    // KNN query is isolated in a subquery first; the JOIN on arguments
    // happens outside so SQLite can push the MATCH constraint correctly.
    // vec0 uses L2 distance. For unit vectors (BGE-micro-v2):
    //   cos_sim = 1 - L2_dist² / 2
    //   cos_sim < 0.35 ↔ L2_dist > sqrt(2 * 0.65) ≈ 1.14
    if let Some(ref vb) = vec_bytes {
        let lowest_dist: f32 = conn
            .query_row(
                "SELECT v.distance \
                 FROM ( \
                     SELECT cmd_path, distance \
                     FROM commands_vec \
                     WHERE embedding MATCH :query_vector AND k = 100 \
                 ) v \
                 JOIN arguments arg ON v.cmd_path = arg.cmd_path \
                 WHERE arg.node_type = 'root' \
                 ORDER BY v.distance ASC \
                 LIMIT 1",
                rusqlite::named_params! { ":query_vector": vb },
                |row| row.get(0),
            )
            .unwrap_or(f32::MAX);

        let is_test =
            std::env::var("CMDH_TEST").is_ok() || std::env::var("CARGO_MANIFEST_DIR").is_ok();
        // Only bail to exact-only results when the query is genuinely unmatched: far in
        // vector space AND no keyword match of any kind. If FTS matches (and/or), proceed
        // to hybrid ranking so verbose intent queries still resolve to a subcommand.
        if !is_test && lowest_dist > 1.14 && !and_match && !or_match {
            return Ok(exact_results);
        }
    }

    // Stage 1: select top 5 app_ids by 3-way Reciprocal Rank Fusion of FTS + vector +
    // POPULARITY, with name dedup. Popularity is a third ranker over the (relevance-gated)
    // candidate set: among the tools that match the query, the most widely-packaged one
    // (apps.popularity, cross-ecosystem repo-count from the Repology dump) gets the best
    // popularity rank. This lifts the canonical tool for brand/concept words (az for
    // "azure", kubectl for "kubernetes") even when its name/path only weakly matches —
    // a pure relevance multiplier can't, because a deep FTS rank stays tiny when scaled.
    // No hardcoded vendor list; new sources just need their popularity column filled.
    // The FTS candidate limit is widened (300) so canonical-but-weak-match tools enter
    // the pool where the popularity ranker can promote them.
    // BM25 weights: cmd_path=0 (unindexed), name=5.0, capabilities=2.0.
    // Raising capabilities weight helps description-based queries (e.g. "knowledge" → obsidian).
    //
    // Popularity weight is gated by query type: a single bare token is a brand/concept
    // lookup ("azure", "kubernetes") where no tool is strongly relevant, so popularity must
    // dominate to surface the canonical CLI; a multi-token query is a task description
    // ("convert an image to equations") where one tool is strongly relevant, so popularity
    // must only nudge — otherwise it would bury a correct niche tool (vectomancy) under a
    // more widely-packaged but wrong one.
    let qtok_n = content_tokens(query).len();
    let pop_w: f64 = if qtok_n <= 1 { 1.0 } else { 0.15 };
    let mut top_apps = Vec::new();
    if let Some(ref vb) = vec_bytes {
        let mut app_stmt = conn.prepare(
            "WITH fts_matched AS ( \
                SELECT cmd_path, row_number() OVER (ORDER BY bm25(apps_fts, 0.0, 5.0, 2.0) ASC) as fts_pos \
                FROM apps_fts WHERE apps_fts MATCH :query LIMIT 300 \
            ), \
            fts_ordered AS ( \
                SELECT arg.app_id, MIN(m.fts_pos) as fts_pos \
                FROM fts_matched m JOIN arguments arg ON m.cmd_path = arg.cmd_path \
                GROUP BY arg.app_id \
            ), \
            vec_knn AS ( \
                SELECT cmd_path, distance FROM commands_vec \
                WHERE embedding MATCH :query_vector AND k = 200 \
            ), \
            vec_rank AS ( \
                SELECT arg.app_id, row_number() OVER (ORDER BY vk.distance ASC) as vec_pos \
                FROM vec_knn vk JOIN arguments arg ON vk.cmd_path = arg.cmd_path \
                WHERE arg.node_type = 'root' \
            ), \
            pre_scored AS ( \
                SELECT \
                    COALESCE(fts.app_id, vec.app_id) as app_id, \
                    fts.fts_pos as fts_pos, vec.vec_pos as vec_pos \
                FROM (SELECT app_id FROM fts_ordered UNION SELECT app_id FROM vec_rank) u \
                LEFT JOIN fts_ordered fts ON u.app_id = fts.app_id \
                LEFT JOIN vec_rank vec ON u.app_id = vec.app_id \
            ), \
            pop_ranked AS ( \
                SELECT ps.app_id, ps.fts_pos, ps.vec_pos, a.name as nm, \
                       dense_rank() OVER (ORDER BY COALESCE(a.popularity, 0.0) DESC) as pop_pos \
                FROM pre_scored ps JOIN apps a ON ps.app_id = a.app_id \
            ), \
            scored AS ( \
                SELECT app_id, nm, \
                       COALESCE(1.0 / (60.0 + fts_pos), 0.0) \
                       + COALESCE(1.0 / (60.0 + vec_pos), 0.0) \
                       + :pop_w / (60.0 + pop_pos) as rrf_score \
                FROM pop_ranked \
            ), \
            name_deduped AS ( \
                SELECT app_id, rrf_score, \
                       row_number() OVER (PARTITION BY nm ORDER BY rrf_score DESC) as rn \
                FROM scored \
            ) \
            SELECT app_id FROM name_deduped WHERE rn = 1 ORDER BY rrf_score DESC LIMIT 5"
        )?;

        let app_rows = app_stmt.query_map(
            rusqlite::named_params! {
                ":query": &processed_query,
                ":query_vector": vb,
                ":pop_w": pop_w,
            },
            |row| row.get::<_, String>(0),
        )?;

        for app_id in app_rows.flatten() {
            top_apps.push(app_id);
        }
    } else {
        let mut app_stmt = conn.prepare(
            "WITH fts_matched AS ( \
                SELECT cmd_path, row_number() OVER (ORDER BY bm25(apps_fts, 0.0, 5.0, 2.0) ASC) as fts_pos \
                FROM apps_fts WHERE apps_fts MATCH :query LIMIT 300 \
            ), \
            fts_ordered AS ( \
                SELECT arg.app_id, MIN(m.fts_pos) as fts_pos \
                FROM fts_matched m JOIN arguments arg ON m.cmd_path = arg.cmd_path \
                GROUP BY arg.app_id \
            ), \
            pop_ranked AS ( \
                SELECT fo.app_id, fo.fts_pos, a.name as nm, \
                       dense_rank() OVER (ORDER BY COALESCE(a.popularity, 0.0) DESC) as pop_pos \
                FROM fts_ordered fo JOIN apps a ON fo.app_id = a.app_id \
            ), \
            scored AS ( \
                SELECT app_id, nm, \
                       (1.0 / (60.0 + fts_pos)) + :pop_w / (60.0 + pop_pos) as rrf_score \
                FROM pop_ranked \
            ), \
            name_deduped AS ( \
                SELECT app_id, rrf_score, \
                       row_number() OVER (PARTITION BY nm ORDER BY rrf_score DESC) as rn \
                FROM scored \
            ) \
            SELECT app_id FROM name_deduped WHERE rn = 1 ORDER BY rrf_score DESC LIMIT 5"
        )?;

        let app_rows = app_stmt.query_map(
            rusqlite::named_params! {
                ":query": &processed_query,
                ":pop_w": pop_w,
            },
            |row| row.get::<_, String>(0),
        )?;

        for app_id in app_rows.flatten() {
            top_apps.push(app_id);
        }
    }

    if top_apps.is_empty() {
        return Ok(exact_results);
    }

    // Guarantee: any app that has an FTS5 text match must appear in top_apps.
    // The combined RRF can push a weak FTS5 match (e.g. obsidian for "knowledge")
    // below the top-5 cutoff when vector-boosted github apps dominate.
    // Fix: collect all FTS5-matching app_ids and inject any that are missing.
    if processed_query != "*" {
        let mut fts_only_stmt = conn.prepare(
            "WITH fts_matched AS ( \
                SELECT cmd_path FROM apps_fts WHERE apps_fts MATCH :query LIMIT 100 \
            ) \
            SELECT DISTINCT arg.app_id \
            FROM fts_matched m JOIN arguments arg ON m.cmd_path = arg.cmd_path \
            LIMIT 5",
        )?;
        let fts_app_rows = fts_only_stmt.query_map(
            rusqlite::named_params! { ":query": &processed_query },
            |row| row.get::<_, String>(0),
        )?;
        for app_id in fts_app_rows.flatten() {
            if !top_apps.contains(&app_id) {
                top_apps.push(app_id);
            }
        }
        top_apps.truncate(8); // cap at 8 to keep Stage 2 bounded
    }

    // Pad top_apps to length 8 for Stage 2 named params
    while top_apps.len() < 8 {
        top_apps.push(top_apps[0].clone());
    }

    // Popularity prior for the selected apps, reused in the Stage-2 re-rank so the
    // within-app command ordering also favours canonical tools (data-driven, no prefixes).
    let mut pop_map: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    {
        let mut uniq: Vec<&String> = top_apps.iter().collect();
        uniq.sort();
        uniq.dedup();
        let placeholders = uniq.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        if let Ok(mut pstmt) = conn.prepare(&format!(
            "SELECT app_id, COALESCE(popularity, 0.0) FROM apps WHERE app_id IN ({placeholders})"
        )) {
            let params = rusqlite::params_from_iter(uniq.iter().map(|s| s.as_str()));
            if let Ok(rows) = pstmt.query_map(params, |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            }) {
                for kv in rows.flatten() {
                    pop_map.insert(kv.0, kv.1);
                }
            }
        }
    }

    // Stage 2: Scoped search.
    // Over-fetch a candidate pool so the leaf-match re-ranker (below) has room to
    // promote the command whose name most precisely matches the query intent.
    let pool = std::cmp::max(limit, 30);
    let mut results = Vec::new();
    if let Some(ref vb) = vec_bytes {
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
                app.os_aliases, \
                app.install_instructions, \
                arg.docker_image, \
                arg.script_url, \
                arg.source_url \
            FROM arguments arg \
            JOIN apps app ON arg.app_id = app.app_id \
            LEFT JOIN fts_rank fts ON arg.cmd_path = fts.cmd_path \
            LEFT JOIN vec_rank vec ON arg.cmd_path = vec.cmd_path \
            WHERE (fts.cmd_path IS NOT NULL OR vec.cmd_path IS NOT NULL) \
              AND arg.app_id IN (:app1, :app2, :app3, :app4, :app5, :app6, :app7, :app8) \
            ORDER BY COALESCE(1.0 / (60.0 + fts.fts_pos), 0.0) + COALESCE(1.0 / (60.0 + vec.vec_pos), 0.0) DESC \
            LIMIT :limit_num"
        )?;

        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":query": &processed_query,
                ":query_vector": vb,
                ":app1": &top_apps[0],
                ":app2": &top_apps[1],
                ":app3": &top_apps[2],
                ":app4": &top_apps[3],
                ":app5": &top_apps[4],
                ":app6": &top_apps[5],
                ":app7": &top_apps[6],
                ":app8": &top_apps[7],
                ":limit_num": pool,
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
                    os_aliases: row.get(7)?,
                    install_instructions: row.get(8)?,
                    docker_image: row.get(9)?,
                    script_url: row.get(10)?,
                    source_url: row.get(11)?,
                })
            },
        )?;

        for r in rows {
            let record = r?;
            if let Ok(contract) = AciCommandContract::try_from(record) {
                results.push(contract);
            }
        }
    } else {
        let mut stmt = conn.prepare(
            "SELECT \
                arg.app_id, \
                app.name, \
                arg.cmd_path, \
                arg.node_type, \
                arg.description, \
                arg.risk_level, \
                arg.example_template, \
                app.os_aliases, \
                app.install_instructions, \
                arg.docker_image, \
                arg.script_url, \
                arg.source_url \
            FROM arguments arg \
            JOIN apps app ON arg.app_id = app.app_id \
            JOIN apps_fts fts ON arg.cmd_path = fts.cmd_path \
            WHERE apps_fts MATCH :query \
              AND arg.app_id IN (:app1, :app2, :app3, :app4, :app5, :app6, :app7, :app8) \
            ORDER BY bm25(apps_fts, 0.0, 5.0, 2.0) ASC \
            LIMIT :limit_num",
        )?;

        let rows = stmt.query_map(
            rusqlite::named_params! {
                ":query": &processed_query,
                ":app1": &top_apps[0],
                ":app2": &top_apps[1],
                ":app3": &top_apps[2],
                ":app4": &top_apps[3],
                ":app5": &top_apps[4],
                ":app6": &top_apps[5],
                ":app7": &top_apps[6],
                ":app8": &top_apps[7],
                ":limit_num": pool,
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
                    os_aliases: row.get(7)?,
                    install_instructions: row.get(8)?,
                    docker_image: row.get(9)?,
                    script_url: row.get(10)?,
                    source_url: row.get(11)?,
                })
            },
        )?;

        for r in rows {
            let record = r?;
            if let Ok(contract) = AciCommandContract::try_from(record) {
                results.push(contract);
            }
        }
    }

    // Path-match re-rank: blend the hybrid-RRF order with how precisely each command's
    // path (service + leaf, e.g. "ec2 create-vpc") matches the query intent. This lifts
    // `aws.ec2.create-vpc` above `aws.apigatewayv2.create-vpc-link` for "create a vpc on
    // aws", and keeps the named service (ec2) in play — without discarding semantic
    // (vector) ranking, since the RRF position still contributes.
    let q_tokens = content_tokens(query);
    if !q_tokens.is_empty() && results.len() > 1 {
        let n = results.len() as i32;
        // Path-match disambiguation is decisive for action/resource queries (multiple
        // tokens, e.g. "create a vpc on aws" → aws.ec2.create-vpc) but harmful for a bare
        // brand/concept word (e.g. "azure"): there a niche tool with the word literally in
        // its path (prowler.azure) would bury the canonical CLI (az), whose identity lives
        // in its popularity/topics, not its command path. So weight path-match strongly only
        // when the query is specific, and lean on the popularity prior for single tokens.
        let path_w = if q_tokens.len() >= 2 { 4 } else { 1 };
        // Popularity bonus mirrors the Stage-1 gating: decisive for a bare brand token,
        // a gentle nudge for descriptive queries so it can't bury a correct niche tool.
        let pop_bonus_w = if q_tokens.len() <= 1 { 50.0 } else { 3.0 };
        let mut scored: Vec<(i32, usize, AciCommandContract)> = results
            .drain(..)
            .enumerate()
            .map(|(i, c)| {
                let rrf = n - i as i32; // higher = better original rank
                let pop_bonus =
                    (pop_bonus_w * pop_map.get(&c.app_id).copied().unwrap_or(0.0)) as i32;
                // Strong boost for root commands on brand lookups so subcommands don't bury them
                let root_bonus = if matches!(c.node_type, cmdhub_shared::NodeType::Root)
                    && q_tokens.len() <= 1
                {
                    20
                } else {
                    0
                };
                let composite = rrf
                    + path_w * path_match_score(&c.cmd_path, &q_tokens)
                    + pop_bonus
                    + root_bonus;
                (composite, i, c)
            })
            .collect();
        // Sort by composite desc; original position as stable tiebreaker.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        results = scored.into_iter().map(|(_, _, c)| c).collect();
    }

    let mut final_results = exact_results.clone();
    final_results.append(&mut results);

    // Deduplicate by cmd_path (same command from multiple sources, e.g. org.archlinux.nb +
    // org.tldr.nb), AND cap results per app so one tool's subcommands can't flood the list:
    // a brand/concept query like "azure" must surface the Azure CLI, not 7 of prowler's
    // "prowler azure ..." checks. Up to PER_APP_CAP keeps "aws ec2 create-vpc/create-subnet"
    // working while leaving room for other tools.
    const PER_APP_CAP: usize = 3;
    let mut seen_paths = std::collections::HashSet::new();
    let mut per_app: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    final_results.retain(|r| {
        if !seen_paths.insert(r.cmd_path.clone()) {
            return false;
        }
        let n = per_app.entry(r.app_id.clone()).or_insert(0);
        *n += 1;
        *n <= PER_APP_CAP
    });

    final_results.truncate(limit);
    Ok(final_results)
}

/// Lowercased content tokens of a query (alphanumerics, stop-words removed).
fn content_tokens(query: &str) -> std::collections::HashSet<String> {
    let stop: std::collections::HashSet<&str> = [
        "how", "to", "a", "the", "on", "in", "of", "for", "with", "an", "is", "at", "by", "and",
        "or", "from", "my", "your", "our", "me", "us", "i", "want", "know", "using", "use", "do",
        "can", "get", "please", "help",
    ]
    .iter()
    .cloned()
    .collect();
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| !stop.contains(w.as_str()))
        .collect()
}

/// Score how well a command's path (service + leaf, excluding the binary) matches the
/// query intent: reward overlap with query tokens, lightly penalise extra path tokens.
/// e.g. for query {create,vpc,aws}: "aws.ec2.create-vpc" → ec2/create/vpc overlap 2,
/// extra 1 (ec2) → 5; "aws.apigatewayv2.create-vpc-link" → overlap 2, extra 2 → 4.
fn path_match_score(cmd_path: &str, q_tokens: &std::collections::HashSet<String>) -> i32 {
    // Drop the first segment (the binary, e.g. "aws") — it's already how we got here.
    let after_binary = cmd_path.split_once('.').map(|x| x.1).unwrap_or(cmd_path);
    let tokens: Vec<String> = after_binary
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return 0;
    }
    let overlap = tokens.iter().filter(|t| q_tokens.contains(*t)).count() as i32;
    let extra = tokens.len() as i32 - overlap;
    3 * overlap - extra
}

pub fn search_commands(
    conn: &Connection,
    query: &str,
    query_vector: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<AciCommandContract>> {
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

    search_cascading(conn, query, query_vector, limit, has_vector_db)
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
            v_bytes.extend_from_slice(&val.to_le_bytes());
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
