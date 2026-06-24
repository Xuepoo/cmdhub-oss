//! cmdh-extractor — local-first recursive command extractor and database generator.
//! Scrapes CLI subcommands and options by recursively parsing help output streams,
//! baking the resulting ACI contracts into the SQLite database.

use anyhow::{Context, Result};
use cmdhub_cli::db::{init_db, open_db};
use cmdhub_shared::{AciCommandContract, NodeType, RiskLevel};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::process::Stdio;
use std::time::Duration;

#[derive(Deserialize, Debug)]
struct Target {
    name: String,
    path: String,
}

#[derive(Deserialize, Debug)]
struct TargetsConfig {
    targets: Vec<Target>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Resolve XDG config directory targets.json
    let config_dir = cmdhub_cli::config::get_config_dir();
    let targets_path = config_dir.join("targets.json");

    let targets = if targets_path.exists() {
        println!("Loading scan targets from config: {:?}", targets_path);
        let content =
            fs::read_to_string(&targets_path).context("Failed to read targets.json file")?;
        let parsed: TargetsConfig =
            serde_json::from_str(&content).context("Failed to parse targets.json")?;
        parsed.targets
    } else {
        println!("No targets.json found. Initializing with default system CLI binaries...");
        vec![
            Target {
                name: "echo".to_string(),
                path: "echo".to_string(),
            },
            Target {
                name: "pwd".to_string(),
                path: "pwd".to_string(),
            },
            Target {
                name: "git".to_string(),
                path: "git".to_string(),
            },
        ]
    };

    // 2. Open and guarantee local database is initialized
    let conn = open_db().context("Failed to open local database")?;
    init_db(&conn).context("Failed to initialize database tables")?;

    // 3. Scrape each target recursively
    for target in targets {
        println!(
            "\n=== Scraping Target CLI: {} ({}) ===",
            target.name, target.path
        );
        if let Err(e) = scrape_target(&conn, &target).await {
            eprintln!(
                "Warning: Failed to scrape target '{}': {:?}",
                target.name, e
            );
        }
    }

    println!("\nExtraction completed successfully! Test database populated.");
    Ok(())
}

/// Recursively scrapes a CLI executable up to depth 3.
async fn scrape_target(conn: &rusqlite::Connection, target: &Target) -> Result<()> {
    // Insert/Replace Application metadata
    let app_id = format!("org.local.{}", target.name);
    conn.execute(
        "INSERT OR REPLACE INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
        (
            &app_id,
            &target.name,
            Some(format!("{{\"pacman\": \"pacman -S {}\"}}", target.name)),
        ),
    )?;

    let mut visited = HashSet::new();
    // Each pending entry carries (1) its PARENT's help text — to detect a node whose own
    // --help just echoes the parent's (no real subtree) — and (2) the description that
    // trailed this node in the parent's command list, used when the node's own help is
    // that useless global echo.
    let mut pending: Vec<(Vec<String>, NodeType, Option<String>, Option<String>)> =
        vec![(vec![], NodeType::Root, None, None)];

    while let Some((sub_path, node_type, parent_help, list_desc)) = pending.pop() {
        if sub_path.len() > 3 {
            continue; // Force maximum depth 3 to avoid infinite loops
        }

        // Build command arguments
        let mut probe_args = sub_path.clone();
        probe_args.push("--help".to_string());

        let args_ref: Vec<&str> = probe_args.iter().map(|s| s.as_str()).collect();
        let mut help_output = match run_probe(&target.path, &args_ref).await {
            Ok(output) => output,
            Err(e) => {
                // Try fallback to "-h"
                let mut fallback_args = sub_path.clone();
                fallback_args.push("-h".to_string());
                let fallback_ref: Vec<&str> = fallback_args.iter().map(|s| s.as_str()).collect();
                match run_probe(&target.path, &fallback_ref).await {
                    Ok(output) => output,
                    Err(_) => {
                        eprintln!(
                            "Failed to probe command {:?} (timeout/error): {:?}",
                            sub_path, e
                        );
                        continue;
                    }
                }
            }
        };

        // If the help output was corrupted by a failed 'man' exec warning, fallback to "-h"
        if help_output.contains("failed to exec") || help_output.trim().is_empty() {
            let mut fallback_args = sub_path.clone();
            fallback_args.push("-h".to_string());
            let fallback_ref: Vec<&str> = fallback_args.iter().map(|s| s.as_str()).collect();
            if let Ok(output) = run_probe(&target.path, &fallback_ref).await {
                if !output.contains("failed to exec") && !output.trim().is_empty() {
                    help_output = output;
                }
            }
        }

        // Resolve materialized path
        let cmd_path = if sub_path.is_empty() {
            target.name.clone()
        } else {
            format!("{}.{}", target.name, sub_path.join("."))
        };

        if !visited.insert(cmd_path.clone()) {
            continue;
        }

        // Skip fabricated subcommands: a non-root node whose --help is an "unknown
        // command/topic" error (glab/ovs-vsctl/kind exit 0 with such text) is not a real
        // subcommand — don't bake it or recurse into it. The root is always kept.
        if !sub_path.is_empty() && help_is_unknown_command(&help_output) {
            continue;
        }

        // The command path as a space-joined string (e.g. "wrangler d1 create"),
        // used to recognise and skip the title-echo line when picking a description.
        let cmd_str = if sub_path.is_empty() {
            target.name.clone()
        } else {
            format!("{} {}", target.name, sub_path.join(" "))
        };
        // Prefer the parent command-list one-liner (list_desc) for subcommands; root
        // nodes (list_desc = None) extract from their own --help. See pick_description.
        let description = pick_description(&help_output, &cmd_str, list_desc.clone());

        let risk_level = if cmd_path.contains("delete")
            || cmd_path.contains("remove")
            || cmd_path.contains("rm")
            || cmd_path.contains("destroy")
        {
            RiskLevel::Dangerous
        } else if cmd_path.contains("write")
            || cmd_path.contains("create")
            || cmd_path.contains("add")
        {
            RiskLevel::Medium
        } else {
            RiskLevel::Safe
        };

        let contract = AciCommandContract {
            app_id: app_id.clone(),
            name: target.name.clone(),
            cmd_path: cmd_path.clone(),
            node_type,
            description,
            risk_level,
            example_template: Some(if sub_path.is_empty() {
                target.path.clone()
            } else {
                format!("{} {}", target.path, sub_path.join(" "))
            }),
            os_aliases: None,
            install_instructions: None,
            docker_image: None,
            script_url: None,
            source_url: None,
            popularity: 0.0,
            verified: true,
            confidence: "high".to_string(),
        };

        // Insert database contract
        insert_contract(conn, &contract)?;
        println!("Baked ACI Contract: {}", cmd_path);

        // Discover next subcommands inside help text if depth is < 3 — but NOT if this
        // node's help merely echoes its parent's (e.g. `systemctl enable --help` ==
        // `systemctl --help`): recursing there re-lists every sibling and explodes the
        // tree. Such a node stays a valid leaf; we just don't descend into it.
        if sub_path.len() < 2 && !help_is_alias_of_parent(&help_output, parent_help.as_deref()) {
            // Full command path so far (binary + sub_path) drives prefix-style parsing.
            let mut cmd_prefix = vec![target.name.clone()];
            cmd_prefix.extend(sub_path.iter().cloned());
            let discovered = parse_subcommands(&help_output, &cmd_prefix);
            for (sub, sub_desc) in discovered {
                let mut next_path = sub_path.clone();
                next_path.push(sub);
                pending.push((
                    next_path,
                    NodeType::Sub,
                    Some(help_output.clone()),
                    sub_desc,
                ));
            }
        }
    }

    Ok(())
}

static SANDBOX_ENGINE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

fn get_sandbox_engine() -> Option<&'static String> {
    SANDBOX_ENGINE
        .get_or_init(|| {
            if std::env::var("CMDH_NO_SANDBOX").is_ok() {
                eprintln!("[INFO] Sandbox explicitly disabled via CMDH_NO_SANDBOX");
                return None;
            }

            // Try podman
            let podman_check = std::process::Command::new("podman")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if let Ok(status) = podman_check {
                if status.success() {
                    eprintln!("[INFO] Sandbox engine detected: podman");
                    return Some("podman".to_string());
                }
            }

            // Try docker
            let docker_check = std::process::Command::new("docker")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if let Ok(status) = docker_check {
                if status.success() {
                    eprintln!("[INFO] Sandbox engine detected: docker");
                    return Some("docker".to_string());
                }
            }

            eprintln!("[WARNING] No sandbox engine (podman/docker) found. Running in unsafe mode!");
            None
        })
        .as_ref()
}

static RUNSC_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn is_runsc_available() -> bool {
    *RUNSC_AVAILABLE.get_or_init(|| which("runsc").is_some())
}

fn which(executable: &str) -> Option<std::path::PathBuf> {
    if executable.contains('/') || executable.contains('\\') {
        return std::path::Path::new(executable).canonicalize().ok();
    }
    if let Ok(paths) = std::env::var("PATH") {
        for path in std::env::split_paths(&paths) {
            let p = path.join(executable);
            if p.exists() && p.is_file() {
                if let Ok(canon) = p.canonicalize() {
                    return Some(canon);
                }
            }
        }
    }
    None
}

/// Spawns the CLI child process probe under a strict 5 second timeout and null stdin constraints.
async fn run_probe(executable: &str, args: &[&str]) -> Result<String> {
    let mut cmd = if let Some(engine) = get_sandbox_engine() {
        let abs_path = which(executable).ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to resolve absolute path for executable: {}",
                executable
            )
        })?;

        let mut run_args = vec!["run".to_string(), "--rm".to_string()];

        if is_runsc_available() {
            run_args.push("--runtime".to_string());
            run_args.push("runsc".to_string());
        }

        run_args.push("--network".to_string());
        run_args.push("none".to_string());

        // Standard system dynamic link directories
        let mut mounts = vec![
            "/usr:/usr:ro".to_string(),
            "/lib:/lib:ro".to_string(),
            "/bin:/bin:ro".to_string(),
        ];

        if std::path::Path::new("/lib64").exists() {
            mounts.push("/lib64:/lib64:ro".to_string());
        }
        if std::path::Path::new("/sbin").exists() {
            mounts.push("/sbin:/sbin:ro".to_string());
        }

        // Mount the parent of the custom executable if not covered by standard mounts
        if let Some(parent) = abs_path.parent() {
            let parent_str = parent.to_string_lossy().to_string();
            if !parent_str.starts_with("/usr")
                && !parent_str.starts_with("/bin")
                && !parent_str.starts_with("/lib")
                && !parent_str.starts_with("/sbin")
            {
                mounts.push(format!("{}:{}:ro", parent_str, parent_str));
            }
        }

        for m in mounts {
            run_args.push("-v".to_string());
            run_args.push(m);
        }

        // Target image (alpine:latest)
        run_args.push("alpine:latest".to_string());

        // Executable inside container (retains absolute host path since parent is mounted at same location)
        run_args.push(abs_path.to_string_lossy().to_string());

        for arg in args {
            run_args.push(arg.to_string());
        }

        let mut child_cmd = tokio::process::Command::new(engine);
        child_cmd.args(&run_args);
        child_cmd
    } else {
        let mut child_cmd = tokio::process::Command::new(executable);
        child_cmd.args(args);
        child_cmd
    };

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to execute command: {}", executable))?;

    let timeout_res = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    match timeout_res {
        Ok(Ok(_status)) => {
            let mut stdout = String::new();
            if let Some(mut out) = child.stdout.take() {
                use tokio::io::AsyncReadExt;
                let _ = out.read_to_string(&mut stdout).await;
            }
            let mut stderr = String::new();
            if let Some(mut err) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = err.read_to_string(&mut stderr).await;
            }

            if !stdout.trim().is_empty() {
                Ok(stdout)
            } else {
                Ok(stderr)
            }
        }
        Ok(Err(e)) => Err(anyhow::anyhow!(e)),
        Err(_) => {
            let _ = child.kill().await;
            Err(anyhow::anyhow!("Process probe timed out (max 5s exceeded)"))
        }
    }
}

/// Simple regex/substring helper to extract subcommands listed in '--help' outputs.
/// True when `line` (already trimmed) is just the command echoed back, optionally
/// followed by placeholder tokens — e.g. `wrangler d1 create <name>` for cmd_str
/// "wrangler d1 create". Such title lines are not descriptions. A line like
/// `git — the stupid content tracker` is NOT a title echo (it carries prose).
fn is_title_echo(line: &str, cmd_str: &str) -> bool {
    match line.strip_prefix(cmd_str) {
        Some(rest) => rest.split_whitespace().all(|t| {
            (t.starts_with('<') && t.ends_with('>')) || (t.starts_with('[') && t.ends_with(']'))
        }),
        None => false,
    }
}

/// Pick a one-line description from a CLI's `--help`, handling the layouts that
/// otherwise yield the command echoed back instead of prose:
/// - **colon style** (az): `az storage : Manage Azure Cloud Storage resources.`
///   → the text after " : ".
/// - **title-then-prose** (wrangler): the first line is the command echo
///   (`wrangler d1 create <name>`), the real summary is the next prose line.
///
/// Section headers (`COMMANDS`, all-caps) and flag lines are skipped.
fn extract_description(help_text: &str, cmd_str: &str) -> String {
    // Colon style: a line that starts with the command path and has " : <desc>".
    for line in help_text.lines() {
        let t = line.trim();
        if t.starts_with(cmd_str) {
            if let Some(idx) = t.find(" : ") {
                let d = t[idx + 3..].trim();
                if !d.is_empty() {
                    return d.to_string();
                }
            }
        }
    }
    // Otherwise: first prose line that isn't the title echo, an all-caps section
    // header, or a flag/positional line.
    for line in help_text.lines() {
        let t = line.trim();
        if t.is_empty() || is_title_echo(t, cmd_str) || t.starts_with('-') {
            continue;
        }
        let is_caps_header = t
            .chars()
            .all(|c| c.is_uppercase() || c.is_whitespace() || c == '&')
            && t.chars().any(|c| c.is_alphabetic());
        if is_caps_header {
            continue;
        }
        return t.to_string();
    }
    "Local subcommand shortcut".to_string()
}

/// Parse subcommand names from a CLI's `--help`, handling two common layouts:
///
/// 1. **Conventional block** (git/docker/az): a `Commands:` / `Subcommands:` /
///    `Subgroups:` header followed by indented `  name   description` lines.
/// 2. **Prefix style** (wrangler and other oclif/yargs CLIs): commands are listed
///    under UPPERCASE section headers *without* a colon, as
///    `  <full path> <sub> [args]   <emoji> desc`, e.g. `  wrangler d1 create <name>`.
///    The bare first word is the binary name, so we strip the known command prefix
///    and take the next token. (This was why wrangler/az probed to 0 subcommands.)
///
/// `cmd_prefix` is the materialized command path so far (e.g. `["wrangler", "d1"]`);
/// it drives the prefix-style extraction. Pass an empty slice to disable pass 1.
/// True when a node's `--help` output is identical to its parent's — the signature of
/// a tool that echoes the same global help for every subcommand (e.g. `systemctl
/// enable --help` == `systemctl --help`). Such a node has no real subtree, so recursing
/// into it would re-discover every sibling and explode the tree (89 subs -> 89x89).
fn help_is_alias_of_parent(this_help: &str, parent_help: Option<&str>) -> bool {
    parent_help == Some(this_help)
}

/// True when a node's `--help` body is actually an "invalid command" error rather than
/// real help. Some tools (glab, ovs-vsctl, kind) exit 0 but print e.g. "Unknown help
/// topic [`x`]" / "unknown command" for a non-existent subcommand — the discovery pass
/// then bakes a fake subcommand whose description is that error string. Skip those.
fn help_is_unknown_command(help: &str) -> bool {
    let low = help.to_lowercase();
    low.contains("unknown help topic")
        || low.contains("unknown command")
        || low.contains("unknown subcommand")
}

/// Choose a command's description. The parent command-list one-liner (`list_desc`,
/// e.g. "Connect to Tailscale") is the canonical short description, so prefer it for a
/// subcommand — the node's own --help is frequently an echo of the global help
/// (systemctl), an error ("xray api --help: unknown command"), or a verbose usage
/// block. Root nodes have no list_desc, so they extract from their own --help.
fn pick_description(own_help: &str, cmd_str: &str, list_desc: Option<String>) -> String {
    match list_desc {
        Some(d) => d,
        None => extract_description(own_help, cmd_str),
    }
}

/// Extract the description that trails a command-list entry, after the subcommand
/// token and any arg placeholders (`enable [UNIT...]   Enable one or more units` ->
/// "Enable one or more units"). The desc starts at the first run of 2+ spaces, the
/// column gutter help formats use to separate name+args from prose.
fn entry_description(rest_after_name: &str) -> Option<String> {
    let idx = rest_after_name.find("  ")?;
    // Strip a leading separator glyph some tools use between name+args and the prose:
    // hyprctl uses "→" ("activewindow   → Gets the active window..."), others "-"/":".
    let desc = rest_after_name[idx..]
        .trim()
        .trim_start_matches(['→', '-', ':'])
        .trim();
    if desc.is_empty() {
        None
    } else {
        Some(desc.to_string())
    }
}

fn parse_subcommands(help_text: &str, cmd_prefix: &[String]) -> Vec<(String, Option<String>)> {
    let mut subcommands: Vec<(String, Option<String>)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let valid = |w: &str| -> bool {
        !w.is_empty()
            && !w.starts_with('-')
            && w.chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            && w != "See"
            && w != "EXPERIMENTAL"
    };
    let mut push = |w: &str, desc: Option<String>, out: &mut Vec<(String, Option<String>)>| {
        if valid(w) && seen.insert(w.to_string()) {
            out.push((w.to_string(), desc));
        }
    };

    // Pass 1 — prefix style: indented lines that repeat the full command path, then
    // the subcommand (`  wrangler d1 create <name>  ...` -> "create").
    if !cmd_prefix.is_empty() {
        let prefix = format!("{} ", cmd_prefix.join(" "));
        for line in help_text.lines() {
            if !(line.starts_with(' ') || line.starts_with('\t')) {
                continue; // entries are always indented; skip the title/usage line
            }
            if let Some(rest) = line.trim_start().strip_prefix(&prefix) {
                if let Some(tok) = rest.split_whitespace().next() {
                    let tok = tok.trim_end_matches(',').trim_end_matches(':');
                    let after =
                        rest[rest.find(tok).map(|i| i + tok.len()).unwrap_or(0)..].to_string();
                    push(tok, entry_description(&after), &mut subcommands);
                }
            }
        }
    }

    // Pass 2 — conventional block style. Re-evaluate at each `Header:` line so
    // multiple command sections (e.g. az's `Subgroups:` + `Commands:`) are all read
    // while non-command sections (`Options:` / `Flags:` / `Arguments:`) end the block.
    let mut in_section = false;
    // Indent of the first entry in the current section. A command entry sits at this
    // shallow indent; a wrapped description continuation ("  enable …,\n        based
    // on preset") is indented to the description column (much deeper) — reject those so
    // their first word ("based"/"ordered") isn't taken as a subcommand.
    let mut section_indent: Option<usize> = None;
    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Section headers, un-indented, in three forms: trailing colon ("Commands:",
        // az "Subgroups:"), angle-bracketed ("<Commands>"/"<Switches>", 7-Zip), or an
        // all-caps bare word ("SUBCOMMANDS"/"FLAGS", tailscale & many Go CLIs). The
        // all-caps form requires every letter uppercase and <=3 words so it can't match
        // a normal prose/usage line.
        let is_all_caps_header = {
            let words: Vec<&str> = trimmed.split_whitespace().collect();
            !words.is_empty()
                && words.len() <= 3
                && trimmed.chars().any(|c| c.is_ascii_uppercase())
                && trimmed
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == ' ' || c == '-')
        };
        let is_header = !line.starts_with(' ')
            && !line.starts_with('\t')
            && (trimmed.ends_with(':')
                || (trimmed.starts_with('<') && trimmed.ends_with('>'))
                || is_all_caps_header);
        if is_header {
            let h = trimmed
                .trim_end_matches(':')
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_lowercase();
            in_section = h.contains("command") || h.contains("subgroup");
            section_indent = None; // new section -> re-establish the entry indent
            continue;
        }
        if in_section && (line.starts_with(' ') || line.starts_with('\t')) {
            let indent = line.len() - line.trim_start().len();
            let base = *section_indent.get_or_insert(indent);
            if indent > base {
                continue; // deeper than the entry column -> wrapped continuation line
            }
            // Comma-separated bare-word list (npm "access, adduser, audit, ci,"): the
            // WHOLE line is command tokens separated by commas, no description column.
            // Detected when the trimmed line has a comma and every comma-part is a single
            // valid token (no spaces -> not prose). Split and push them all, then move on.
            if trimmed.contains(',') {
                let parts: Vec<&str> = trimmed
                    .split(',')
                    .map(|p| p.trim())
                    .filter(|p| !p.is_empty())
                    .collect();
                if !parts.is_empty() && parts.iter().all(|p| !p.contains(' ') && valid(p)) {
                    for p in parts {
                        push(p, None, &mut subcommands);
                    }
                    continue;
                }
            }
            if let Some(first_raw) = line.split_whitespace().next() {
                let first = first_raw.trim_end_matches(',').trim_end_matches(':');
                // Skip lines that begin with the binary name — those are prefix-style
                // entries already handled by pass 1.
                let is_prefix_line = cmd_prefix
                    .first()
                    .map(|b| first == b.as_str())
                    .unwrap_or(false);
                if !is_prefix_line {
                    let entry = line.trim_start();
                    let after = &entry[entry.find(first).map(|i| i + first.len()).unwrap_or(0)..];
                    // Two entry shapes: colon-separated ("a : Add files", "name :
                    // desc") -> everything after " :" is the description; or
                    // column-gutter ("enable [UNIT...]   Enable …") -> desc starts at
                    // the first 2-space run (entry_description).
                    let desc = if let Some(rest) = after.trim_start().strip_prefix(": ") {
                        let d = rest.trim();
                        (!d.is_empty()).then(|| d.to_string())
                    } else if let Some(rest) = after.strip_prefix(" :") {
                        // "name [Preview] : desc" — colon not immediately after name
                        let d = rest.trim();
                        (!d.is_empty()).then(|| d.to_string())
                    } else {
                        entry_description(after)
                    };
                    push(first, desc, &mut subcommands);
                }
            }
        }
    }

    subcommands
}

/// Inserts ACI contract, FTS5 data, and mock zero vectors into SQLite.
fn insert_contract(conn: &rusqlite::Connection, contract: &AciCommandContract) -> Result<()> {
    let (app, arg) = contract.to_db_records()?;

    conn.execute(
        "INSERT OR REPLACE INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template, provenance) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'probe')",
        (
            &arg.cmd_path,
            &arg.app_id,
            &arg.node_name,
            &arg.node_type,
            &arg.description,
            &arg.risk_level,
            &arg.example_template,
        ),
    )?;

    // Safe delete then insert to prevent OR REPLACE virtual table issues in FTS5
    let _ = conn.execute(
        "DELETE FROM apps_fts WHERE cmd_path = ?1",
        rusqlite::params![&arg.cmd_path],
    );
    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
        (&arg.cmd_path, &app.name, &arg.description),
    )?;

    // Populate a placeholder float32[384] embedding (1536 bytes, little-endian) to
    // match the vec0(float[384]) schema. build_db re-embeds every row, so the value
    // is irrelevant — it only has to be the right dimension to insert. A unit-ish
    // first component avoids any RRF division-by-zero before the real embed.
    let mut emb = vec![0f32; 384];
    emb[0] = 1.0;
    let mut vec_bytes = Vec::with_capacity(384 * 4);
    for v in &emb {
        vec_bytes.extend_from_slice(&v.to_le_bytes());
    }

    // Safe delete then insert to prevent OR REPLACE virtual table issues in sqlite-vec
    let _ = conn.execute(
        "DELETE FROM commands_vec WHERE cmd_path = ?1",
        rusqlite::params![&arg.cmd_path],
    );
    conn.execute(
        "INSERT INTO commands_vec (cmd_path, embedding) VALUES (?1, ?2)",
        rusqlite::params![&arg.cmd_path, vec_bytes],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: drop descriptions, keep just the subcommand names for assertions.
    fn names(subs: Vec<(String, Option<String>)>) -> Vec<String> {
        subs.into_iter().map(|(n, _)| n).collect()
    }

    #[test]
    fn test_parse_subcommands_various_formats() {
        let help_text_git = "\
git — the stupid content tracker

Usage: git <command>

Commands:
  clone      Clone a repository into a new directory
  init       Create an empty Git repository or reinitialize an existing one
  add        Add file contents to the index

Options:
  -v, --version  Show version
";
        let subcommands = names(parse_subcommands(help_text_git, &[]));
        assert_eq!(subcommands, vec!["clone", "init", "add"]);

        let help_text_subcommands = "\
Some tool subcommands list.

Subcommands:
  create     Create resource
  delete     Delete resource

Flags:
  -h, --help  Help
";
        let subcommands_2 = names(parse_subcommands(help_text_subcommands, &[]));
        assert_eq!(subcommands_2, vec!["create", "delete"]);

        // Verify it stops parsing on next header like "Options:" or "Flags:"
        let help_text_stop = "\
Commands:
  status     Check status

Options:
  commit     This should not be parsed as a subcommand because it is under Options
";
        let subcommands_3 = names(parse_subcommands(help_text_stop, &[]));
        assert_eq!(subcommands_3, vec!["status"]);
    }

    #[test]
    fn test_parse_subcommands_wrangler_prefix_style() {
        // wrangler: UPPERCASE section headers (no colon) + "wrangler <sub>" prefix + emoji.
        let top = "\
wrangler

COMMANDS
  wrangler docs [search..]        \u{1F4DA} Open Wrangler's command documentation
  wrangler email                  Manage Cloudflare Email services [open beta]

ACCOUNT
  wrangler login                  \u{1F513} Login to Cloudflare

STORAGE
  wrangler d1                     \u{1F5C4} Manage Workers D1 databases
  wrangler r2                     \u{1F4E6} Manage R2 buckets

GLOBAL FLAGS
  -c, --config          Path to config  [string]
";
        let subs = names(parse_subcommands(top, &["wrangler".to_string()]));
        assert_eq!(subs, vec!["docs", "email", "login", "d1", "r2"]);

        // Nested: `wrangler d1 --help` repeats the full path "wrangler d1 <sub>".
        let d1 = "\
wrangler d1

\u{1F5C4} Manage Workers D1 databases

COMMANDS
  wrangler d1 create <name>       Creates a new D1 database
  wrangler d1 list                List all D1 databases in your account
  wrangler d1 delete <name>       Delete a D1 database
  wrangler d1 time-travel         Restore, fork or copy a database

GLOBAL FLAGS
  -h, --help            Show help  [boolean]
";
        let subs = names(parse_subcommands(
            d1,
            &["wrangler".to_string(), "d1".to_string()],
        ));
        assert_eq!(subs, vec!["create", "list", "delete", "time-travel"]);
    }

    #[test]
    fn test_parse_subcommands_az_subgroups() {
        // az: `Subgroups:` + `Commands:` headers, "  name [Preview] : desc" entries.
        let az = "\
Group
    az storage : Manage Azure Cloud Storage resources.

Subgroups:
    account                  : Manage storage accounts.
    blob                     : Manage object storage for unstructured data (blobs).
    message        [Preview] : Manage queue storage messages.

Commands:
    generate-sas             : Generate a shared access signature.

Global Arguments:
    --debug                  : Increase logging verbosity.
";
        let subs = names(parse_subcommands(
            az,
            &["az".to_string(), "storage".to_string()],
        ));
        assert_eq!(subs, vec!["account", "blob", "message", "generate-sas"]);
    }

    #[test]
    fn test_parse_subcommands_skips_wrapped_continuation_lines() {
        // systemctl: command entries at a shallow indent, with descriptions that WRAP
        // onto a deeply-indented continuation line. The continuation's first word
        // ("ordered", "based") must NOT be mistaken for a subcommand.
        let systemctl = "\
Unit Commands:
  list-units [PATTERN...]             List units currently in memory,
                                      ordered by path
  enable [UNIT...]                    Enable one or more units, possibly
                                      based on preset configuration
  daemon-reload                       Reload systemd manager configuration

Options:
  -h --help                           Show help
";
        let subs: Vec<String> = parse_subcommands(systemctl, &["systemctl".to_string()])
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(subs, vec!["list-units", "enable", "daemon-reload"]);
    }

    #[test]
    fn test_parse_subcommands_uppercase_bare_header() {
        // tailscale (and many Go CLIs): an all-caps bare section header "SUBCOMMANDS"
        // (no colon, no angle brackets), then 2-space column-gutter entries.
        let tailscale = "\
USAGE
  tailscale [flags] <command> [command flags]

SUBCOMMANDS
  up           Connect to Tailscale, logging in if needed
  status       Show state of tailscaled and its connections
  ping         Ping a host at the Tailscale layer

FLAGS
  --socket     path to tailscaled socket
";
        let subs = parse_subcommands(tailscale, &["tailscale".to_string()]);
        let names: Vec<String> = subs.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, vec!["up", "status", "ping"]); // --socket under FLAGS excluded
        let up = subs.iter().find(|(n, _)| n == "up").unwrap();
        assert_eq!(
            up.1.as_deref(),
            Some("Connect to Tailscale, logging in if needed")
        );
    }

    #[test]
    fn test_parse_subcommands_angle_bracket_headers() {
        // 7-Zip: section header is "<Commands>" (angle brackets, NO trailing colon),
        // entries are "  a : Add files to archive". "<Switches>" ends the command block.
        let sevenzip = "\
Usage: 7z <command> [<switches>...] <archive_name>

<Commands>
  a : Add files to archive
  d : Delete files from archive
  x : eXtract files with full paths

<Switches>
  -t : Set type of archive
";
        let subs = parse_subcommands(sevenzip, &["7z".to_string()]);
        let names: Vec<String> = subs.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, vec!["a", "d", "x"]); // -t under <Switches> excluded
        let a = subs.iter().find(|(n, _)| n == "a").unwrap();
        assert_eq!(a.1.as_deref(), Some("Add files to archive"));
    }

    #[test]
    fn test_parse_subcommands_captures_inline_descriptions() {
        // Each command-list entry's trailing text is its description — captured so a
        // subcommand whose own --help only echoes the global help (systemctl) still
        // gets a real description from the parent's list.
        let systemctl = "\
Unit Commands:
  list-units [PATTERN...]             List units currently in memory,
                                      ordered by path
  enable [UNIT...]                    Enable one or more units
  daemon-reload                       Reload systemd manager configuration
";
        let subs = parse_subcommands(systemctl, &["systemctl".to_string()]);
        let enable = subs.iter().find(|(n, _)| n == "enable").unwrap();
        assert_eq!(enable.1.as_deref(), Some("Enable one or more units"));
        let dr = subs.iter().find(|(n, _)| n == "daemon-reload").unwrap();
        assert_eq!(
            dr.1.as_deref(),
            Some("Reload systemd manager configuration")
        );
        // entry with a [PATTERN...] arg token before the description (verbatim, incl.
        // the trailing comma where the real help wraps to a continuation line)
        let lu = subs.iter().find(|(n, _)| n == "list-units").unwrap();
        assert_eq!(lu.1.as_deref(), Some("List units currently in memory,"));
    }

    #[test]
    fn test_pick_description_prefers_parent_list_desc() {
        // The parent command-list one-liner is the canonical description. Prefer it for
        // a subcommand over the node's own --help, which is often an echo (systemctl),
        // an error (xray 'unknown command'), or a verbose usage block.
        assert_eq!(
            pick_description(
                "xray api --help: unknown command",
                "xray api",
                Some("Call an API in an Xray process".to_string())
            ),
            "Call an API in an Xray process"
        );
        // No parent list desc (root node) -> delegates to own-help extraction.
        assert_eq!(
            pick_description("Frobnicate all the things\n\nUsage: foo ...", "foo", None),
            "Frobnicate all the things"
        );
    }

    #[test]
    fn test_parse_subcommands_comma_separated_list() {
        // npm: "All commands:" then comma-separated bare words, multiple per (wrapped) line.
        let npm = "\
npm <command>

All commands:

    access, adduser, audit, ci,
    completion, config, install, run,
    test, publish, uninstall, version

Specify configs ...
";
        let subs = parse_subcommands(npm, &["npm".to_string()]);
        let names: Vec<String> = subs.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            names,
            vec![
                "access",
                "adduser",
                "audit",
                "ci",
                "completion",
                "config",
                "install",
                "run",
                "test",
                "publish",
                "uninstall",
                "version"
            ]
        );
    }

    #[test]
    fn test_comma_list_does_not_break_description_commas() {
        // A normal entry whose DESCRIPTION contains commas must NOT be comma-split.
        let help = "\
Commands:
  sync       Download, verify, and install packages
  clean      Remove old, unused files
";
        let subs = parse_subcommands(help, &["pac".to_string()]);
        let names: Vec<String> = subs.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, vec!["sync", "clean"]);
    }

    #[test]
    fn test_parse_subcommands_arrow_separator() {
        // hyprctl: "  name        → description" with an arrow glyph separator.
        let hyprctl = "\
usage: hyprctl [flags] <command>

commands:
    activewindow        → Gets the active window name and its properties
    binds               → Lists all registered binds
    monitors            → Lists active outputs with their properties
";
        let subs = parse_subcommands(hyprctl, &["hyprctl".to_string()]);
        let aw = subs.iter().find(|(n, _)| n == "activewindow").unwrap();
        assert_eq!(
            aw.1.as_deref(),
            Some("Gets the active window name and its properties")
        );
        let names: Vec<String> = subs.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, vec!["activewindow", "binds", "monitors"]);
    }

    #[test]
    fn test_help_is_unknown_command() {
        assert!(help_is_unknown_command(
            "Unknown help topic [`endpoint` `skills`]"
        ));
        assert!(help_is_unknown_command(
            "ovs-vsctl: unknown command 'foo'; use --help"
        ));
        assert!(help_is_unknown_command(
            "Error: unknown subcommand \"bar\" for \"kind\""
        ));
        assert!(!help_is_unknown_command(
            "Connect to Tailscale\n\nUSAGE\n  tailscale up [flags]"
        ));
    }

    #[test]
    fn test_help_is_alias_of_parent() {
        // A subcommand whose `--help` returns the SAME text as its parent (systemctl
        // enable --help == systemctl --help) has no real subtree -> must not recurse.
        let parent = "Unit Commands:\n  enable\n  disable\n";
        assert!(help_is_alias_of_parent(parent, Some(parent)));
        assert!(!help_is_alias_of_parent(
            "different child help",
            Some(parent)
        ));
        assert!(!help_is_alias_of_parent(parent, None)); // root has no parent
    }

    #[test]
    fn test_extract_description_skips_title_echo() {
        // wrangler leaf: title echo "wrangler d1 create <name>" then the real summary.
        let create = "\
wrangler d1 create <name>

Creates a new D1 database, and provides the binding and UUID

POSITIONALS
  name  The name of the new D1 database  [string] [required]
";
        assert_eq!(
            extract_description(create, "wrangler d1 create"),
            "Creates a new D1 database, and provides the binding and UUID"
        );

        // wrangler group: title echo "wrangler d1" then an emoji summary line.
        let d1 = "\
wrangler d1

\u{1F5C4} Manage Workers D1 databases

COMMANDS
  wrangler d1 create <name>  Creates a new D1 database
";
        assert_eq!(
            extract_description(d1, "wrangler d1"),
            "\u{1F5C4} Manage Workers D1 databases"
        );

        // az colon style: description follows " : " on the title line.
        let az = "\
Group
    az storage : Manage Azure Cloud Storage resources.

Subgroups:
    blob : Manage object storage.
";
        assert_eq!(
            extract_description(az, "az storage"),
            "Manage Azure Cloud Storage resources."
        );

        // git: first line is real prose that happens to start with the binary name —
        // must NOT be mistaken for a title echo.
        let git = "\
git — the stupid content tracker

Usage: git <command>
";
        assert_eq!(
            extract_description(git, "git"),
            "git — the stupid content tracker"
        );
    }

    #[tokio::test]
    async fn test_run_probe_successful_execution() {
        // run_probe routes through a container sandbox (docker/podman + alpine) when
        // one is detected. On minimal CI runners that roundtrip can't run — a host
        // glibc binary executed inside musl alpine, or a missing image under
        // `--network none`. Skip there; wherever a working sandbox exists (e.g. a
        // dev box) or none is detected (direct exec) the happy path is still asserted.
        let res = run_probe("echo", &["hello_probe_test"]).await;
        if res.is_err() && get_sandbox_engine().is_some() {
            eprintln!(
                "skipping test_run_probe_successful_execution: sandbox roundtrip unavailable here"
            );
            return;
        }
        let output = res.expect("run_probe should succeed (no sandbox, or sandbox works)");
        assert!(output.contains("hello_probe_test"));
    }

    #[tokio::test]
    async fn test_run_probe_non_existent_binary() {
        let res = run_probe("non_existent_binary_abc_123", &[]).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_run_probe_timeout_limit() {
        // Sleep 10 seconds, which must exceed 5 seconds run_probe limit
        let res = run_probe("sleep", &["10"]).await;
        assert!(res.is_err());
        let err_str = res.unwrap_err().to_string();
        assert!(err_str.contains("timed out") || err_str.contains("max 5s exceeded"));
    }
}
