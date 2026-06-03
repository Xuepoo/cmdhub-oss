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
    let mut pending = vec![(vec![], NodeType::Root)];

    while let Some((sub_path, node_type)) = pending.pop() {
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

        // Extract first line of help output as description
        let description = help_output
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Local subcommand shortcut")
            .trim()
            .to_string();

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
            install_instructions: None,
            docker_image: None,
            script_url: None,
            source_url: None,
        };

        // Insert database contract
        insert_contract(conn, &contract)?;
        println!("Baked ACI Contract: {}", cmd_path);

        // Discover next subcommands inside help text if depth is < 3
        if sub_path.len() < 2 {
            let discovered = parse_subcommands(&help_output);
            for sub in discovered {
                let mut next_path = sub_path.clone();
                next_path.push(sub);
                pending.push((next_path, NodeType::Sub));
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

        let mut run_args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "--network".to_string(),
            "none".to_string(),
        ];

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
fn parse_subcommands(help_text: &str) -> Vec<String> {
    let mut subcommands = Vec::new();
    let mut in_subcommands_section = false;

    for line in help_text.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();

        // Match "commands" or "subcommands" more robustly (e.g. "These are common Git commands:")
        if !in_subcommands_section
            && (lower.contains("commands:")
                || lower.contains("subcommands:")
                || lower.contains("common git commands")
                || (lower.contains("commands") && trimmed.ends_with(':')))
        {
            in_subcommands_section = true;
            continue;
        }

        if in_subcommands_section {
            if trimmed.is_empty() {
                continue; // Skip empty lines inside section
            }
            // If we hit a new header section like "options:" or "flags:", stop parsing
            if trimmed.ends_with(':') && !trimmed.contains(" ") {
                break;
            }

            // Subcommands are always indented in help outputs.
            // Ignore non-indented lines like subcategory headings (e.g., "start a working area")
            if line.starts_with(' ') || line.starts_with('\t') {
                if let Some(first_word_raw) = line.split_whitespace().next() {
                    // Strip trailing commas (e.g., "build, b" -> "build") or semicolons/colons
                    let first_word = first_word_raw.trim_end_matches(',').trim_end_matches(':');
                    if first_word.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                        && !first_word.starts_with('-')
                        && !first_word.is_empty()
                        // Ignore general words that are not actually commands (like experimental headers or notes)
                        && first_word != "See"
                        && first_word != "EXPERIMENTAL"
                    {
                        subcommands.push(first_word.to_string());
                    }
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
        "INSERT OR REPLACE INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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

    // Populate mock unit-embeddings (FLOAT[512]) to prevent RRF query execution division-by-zero
    let mut mock_embedding = vec![0.0f32; 512];
    mock_embedding[0] = 1.0f32;

    let mut vec_bytes = Vec::with_capacity(512 * 4);
    for &val in &mock_embedding {
        vec_bytes.extend_from_slice(&val.to_ne_bytes());
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
        let subcommands = parse_subcommands(help_text_git);
        assert_eq!(subcommands, vec!["clone", "init", "add"]);

        let help_text_subcommands = "\
Some tool subcommands list.

Subcommands:
  create     Create resource
  delete     Delete resource

Flags:
  -h, --help  Help
";
        let subcommands_2 = parse_subcommands(help_text_subcommands);
        assert_eq!(subcommands_2, vec!["create", "delete"]);

        // Verify it stops parsing on next header like "Options:" or "Flags:"
        let help_text_stop = "\
Commands:
  status     Check status

Options:
  commit     This should not be parsed as a subcommand because it is under Options
";
        let subcommands_3 = parse_subcommands(help_text_stop);
        assert_eq!(subcommands_3, vec!["status"]);
    }

    #[tokio::test]
    async fn test_run_probe_successful_execution() {
        let res = run_probe("echo", &["hello_probe_test"]).await;
        assert!(res.is_ok());
        let output = res.unwrap();
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
