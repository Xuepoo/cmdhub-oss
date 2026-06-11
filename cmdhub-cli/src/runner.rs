use crate::config::Config;
use anyhow::{Context, Result};
use cmdhub_shared::{AciCommandContract, CmdHubError, DbAciRecord, RiskLevel};
use rusqlite::Connection;
use std::io::{self, Write};
use std::process::{Command, Stdio};

pub fn get_command_by_path(conn: &Connection, cmd_path: &str) -> Result<AciCommandContract> {
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
            app.popularity, \
            arg.docker_image, \
            arg.script_url, \
            arg.source_url \
        FROM arguments arg \
        JOIN apps app ON arg.app_id = app.app_id \
        WHERE arg.cmd_path = ?1",
    )?;

    let record = stmt
        .query_row([cmd_path], |row| {
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
                popularity: row.get(9)?,
                docker_image: row.get(10)?,
                script_url: row.get(11)?,
                source_url: row.get(12)?,
            })
        })
        .context("Command path not found in database")?;

    AciCommandContract::try_from(record).map_err(|e| anyhow::anyhow!(e))
}

pub fn run_command(
    config: &Config,
    conn: &Connection,
    cmd_path: &str,
    args: &[String],
    skip_gating: bool,
) -> Result<()> {
    let contract = get_command_by_path(conn, cmd_path)?;

    // Safety Gate
    if contract.risk_level == RiskLevel::Dangerous && !skip_gating {
        match config.risk_guard_level.as_str() {
            "allow" => {
                // proceed to execution without prompts or errors
            }
            "block" => {
                return Err(anyhow::anyhow!(CmdHubError::ExecutionBlocked {
                    risk_level: "dangerous".to_string(),
                    command: contract.cmd_path.clone(),
                }));
            }
            _ => {
                // "ask"
                use std::io::IsTerminal;
                if std::env::var("CMD_TEST").is_ok() || !std::io::stdin().is_terminal() {
                    return Err(anyhow::anyhow!(CmdHubError::ExecutionBlocked {
                        risk_level: "dangerous".to_string(),
                        command: contract.cmd_path.clone(),
                    }));
                }

                eprintln!("\x1b[31;1m[WARNING] RISK LEVEL IS DANGEROUS!\x1b[0m");
                eprintln!("\x1b[31;1mThis command may have destructive side effects, file deletions, or privilege escalations.\x1b[0m");
                eprintln!("\x1b[31;1mCommand Path: {}\x1b[0m", contract.cmd_path);
                eprintln!("\x1b[31;1mDescription: {}\x1b[0m", contract.description);
                eprint!("Are you sure you want to execute this command? (y/yes to confirm): ");
                io::stderr().flush()?;

                let mut input = String::new();
                io::stdin()
                    .read_line(&mut input)
                    .context("Failed to read user confirmation from standard input")?;
                let trimmed = input.trim().to_lowercase();
                if trimmed != "y" && trimmed != "yes" {
                    return Err(anyhow::anyhow!(CmdHubError::ExecutionBlocked {
                        risk_level: "dangerous".to_string(),
                        command: contract.cmd_path.clone(),
                    }));
                }
            }
        }
    }

    // Prepare executable
    let executable = crate::dto::resolve_binary_name(&contract, config);

    // Check if installed
    if !crate::dto::check_is_installed(&contract, config) {
        eprintln!(
            "Warning: command '{}' is not installed locally.",
            executable
        );
        if let Some(install_cmd) = crate::dto::resolve_install_command(&contract, config) {
            eprintln!("To install, run: {}", install_cmd);
        }
    }

    // Spawn the subprocess
    let mut child = Command::new(&executable)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to spawn command '{}'", executable))?;

    let exit_status = child
        .wait()
        .context("Failed to wait for child process execution")?;

    if !exit_status.success() {
        if let Some(code) = exit_status.code() {
            return Err(anyhow::anyhow!("Process exited with status code: {}", code));
        } else {
            return Err(anyhow::anyhow!("Process terminated by signal"));
        }
    }

    Ok(())
}
