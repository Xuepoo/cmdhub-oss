//! Model Context Protocol (MCP) server for CmdHub.
//!
//! Exposes ACI search and command execution over standard input/output (Stdio Transport).
//! All logs and error outputs are strictly printed to STDERR to ensure JSON-RPC stream purity on STDOUT.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    arguments: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SearchArguments {
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ExecuteArguments {
    cmd_path: String,
    args: Option<Vec<String>>,
}

struct AppState {
    model: Option<std::sync::Arc<cmdhub_cli::inference::EmbeddingModel>>,
    tokenizer: cmdhub_cli::tokenizer::Tokenizer,
    #[allow(dead_code)]
    config: cmdhub_cli::config::Config,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Initialize tracing to output strictly to STDERR
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .map_err(|e| anyhow::anyhow!("Failed to initialize tracing subscriber: {}", e))?;

    tracing::info!("Starting CmdHub MCP server on stdio transport...");

    let config = cmdhub_cli::config::load_or_create_config(None).unwrap_or_default();

    let model = match cmdhub_cli::installer::ensure_model_installed(&config).await {
        Ok(model_path) => match cmdhub_cli::inference::EmbeddingModel::load(&model_path) {
            Ok(m) => {
                tracing::info!("Loaded embedding model from {:?}", model_path);
                Some(std::sync::Arc::new(m))
            }
            Err(e) => {
                tracing::error!("Failed to load embedding model: {:?}", e);
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                "Failed to ensure embedding model: {}. Local semantic search is disabled.",
                e
            );
            None
        }
    };

    let tokenizer = cmdhub_cli::tokenizer::Tokenizer::new();
    let state = std::sync::Arc::new(AppState {
        model,
        tokenizer,
        config,
    });

    // 2. Main Stdio communication loop
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut line = String::new();

    while handle
        .read_line(&mut line)
        .context("Failed to read line from stdin")?
        > 0
    {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if let Err(e) = handle_json_rpc_message(trimmed, &state) {
                tracing::error!("Error handling message: {:?}", e);
            }
        }
        line.clear();
    }

    tracing::info!("CmdHub MCP server shutting down.");
    Ok(())
}

/// Sends a JSON-RPC response back to the client over STDOUT.
fn send_response<T: Serialize>(resp: &T) -> Result<()> {
    let serialized =
        serde_json::to_string(resp).context("Failed to serialize JSON-RPC response")?;
    let mut stdout = io::stdout();
    stdout
        .write_all(serialized.as_bytes())
        .context("Failed to write to stdout")?;
    stdout
        .write_all(b"\n")
        .context("Failed to write newline to stdout")?;
    stdout.flush().context("Failed to flush stdout")?;
    Ok(())
}

/// Parses and processes a single JSON-RPC line.
fn handle_json_rpc_message(line: &str, state: &AppState) -> Result<()> {
    // Parse JSON-RPC request safely using the JsonRpcRequest type
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            let err_resp = JsonRpcResponse {
                jsonrpc: "2.0",
                id: serde_json::Value::Null,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: format!("Parse error: {}", e),
                    data: None,
                }),
            };
            send_response(&err_resp)?;
            return Ok(());
        }
    };

    if req.jsonrpc != "2.0" {
        let id = req.id.unwrap_or(serde_json::Value::Null);
        let err_resp = JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code: -32600,
                message: "Invalid Request: missing or invalid 'jsonrpc' version (must be '2.0')"
                    .to_string(),
                data: None,
            }),
        };
        send_response(&err_resp)?;
        return Ok(());
    }

    let id = req.id.clone().unwrap_or(serde_json::Value::Null);
    let is_notification = req.id.is_none();

    match req.method.as_str() {
        "initialize" => {
            let resp = JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "cmdhub-mcp",
                        "version": "0.1.0"
                    }
                })),
                error: None,
            };
            send_response(&resp)?;
        }
        "notifications/initialized" => {
            // Standard notification, client confirming initialization. Silent ignore.
        }
        "tools/list" => {
            let resp = JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(serde_json::json!({
                    "tools": [
                        {
                            "name": "cmdhub_search",
                            "description": "Search offline CmdHub database for CLI command based on a natural language intent",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "query": {
                                        "type": "string",
                                        "description": "The natural language query to search for"
                                    },
                                    "limit": {
                                        "type": "integer",
                                        "description": "Maximum number of search results to return (optional)"
                                    }
                                },
                                "required": ["query"]
                            }
                        },
                        {
                            "name": "cmdhub_execute",
                            "description": "Run a specific terminal command by path",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "cmd_path": {
                                        "type": "string",
                                        "description": "The command path to execute (e.g. \"tar.extract\")"
                                    },
                                    "args": {
                                        "type": "array",
                                        "items": {
                                            "type": "string"
                                        },
                                        "description": "Arguments passed directly to the underlying CLI tool (optional)"
                                    }
                                },
                                "required": ["cmd_path"]
                            }
                        }
                    ]
                })),
                error: None,
            };
            send_response(&resp)?;
        }
        "tools/call" => {
            match handle_tool_call(req.params.as_ref(), state) {
                Ok(val) => {
                    let resp = JsonRpcResponse {
                        jsonrpc: "2.0",
                        id,
                        result: Some(val),
                        error: None,
                    };
                    send_response(&resp)?;
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    let err_code = if err_msg.contains("invalid type")
                        || err_msg.contains("missing field")
                        || err_msg.contains("Invalid tool arguments")
                    {
                        -32602 // Invalid params
                    } else {
                        -32603 // Internal error
                    };
                    let resp = JsonRpcResponse {
                        jsonrpc: "2.0",
                        id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: err_code,
                            message: err_msg,
                            data: None,
                        }),
                    };
                    send_response(&resp)?;
                }
            }
        }
        _ => {
            if !is_notification {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32601,
                        message: format!("Method not found: '{}'", req.method),
                        data: None,
                    }),
                };
                send_response(&resp)?;
            }
        }
    }

    Ok(())
}

/// Executes the core logic for MCP tool dispatching.
fn handle_tool_call(
    params: Option<&serde_json::Value>,
    state: &AppState,
) -> Result<serde_json::Value> {
    let params_val =
        params.ok_or_else(|| anyhow::anyhow!("Missing 'params' in 'tools/call' request"))?;
    let tool_call: ToolCallParams = serde_json::from_value(params_val.clone())
        .context("Invalid 'params' format for 'tools/call'")?;

    match tool_call.name.as_str() {
        "cmdhub_search" => {
            let args_val = tool_call
                .arguments
                .ok_or_else(|| anyhow::anyhow!("Missing 'arguments' for 'cmdhub_search'"))?;
            let search_args: SearchArguments = serde_json::from_value(args_val)
                .context("Invalid tool arguments for 'cmdhub_search'")?;

            let conn = cmdhub_cli::db::open_db().context("Failed to open local database")?;
            let _ = cmdhub_cli::db::init_db(&conn);

            let limit = search_args.limit.unwrap_or(1);

            // During search handle, query using the in-memory shared model to guarantee sub-5ms:
            let query_vector = if let Some(ref m) = state.model {
                let (ids, mask) = state.tokenizer.tokenize_query(&search_args.query);
                m.generate_embedding(&ids, &mask).ok()
            } else {
                None
            };

            let results = cmdhub_cli::db::search_all(
                &conn,
                &search_args.query,
                query_vector.as_deref(),
                limit,
            )
            .context("Failed to execute hybrid search query")?;

            let serialized = serde_json::to_string(&results)
                .context("Failed to serialize hybrid search results to JSON")?;

            Ok(serde_json::json!({
                "content": [
                    {
                        "type": "text",
                        "text": serialized
                    }
                ]
            }))
        }
        "cmdhub_execute" => {
            let args_val = tool_call
                .arguments
                .ok_or_else(|| anyhow::anyhow!("Missing 'arguments' for 'cmdhub_execute'"))?;
            let execute_args: ExecuteArguments = serde_json::from_value(args_val)
                .context("Invalid tool arguments for 'cmdhub_execute'")?;

            let conn = cmdhub_cli::db::open_db().context("Failed to open local database")?;
            let _ = cmdhub_cli::db::init_db(&conn);

            let args = execute_args.args.unwrap_or_default();

            // Run command securely redirection-wrapped
            match run_command_safely(&conn, &execute_args.cmd_path, &args) {
                Ok(_) => Ok(serde_json::json!({
                    "content": [
                        {
                            "type": "text",
                            "text": format!("Command executed successfully: {}", execute_args.cmd_path)
                        }
                    ]
                })),
                Err(e) => Ok(serde_json::json!({
                    "content": [
                        {
                            "type": "text",
                            "text": format!("Command execution failed: {:#}", e)
                        }
                    ],
                    "isError": true
                })),
            }
        }
        _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_call.name)),
    }
}

/// Executes a CLI command securely, routing its standard output to standard error to prevent JSON-RPC pollution.
#[cfg(unix)]
fn run_command_safely(conn: &rusqlite::Connection, cmd_path: &str, args: &[String]) -> Result<()> {
    use std::os::unix::io::AsRawFd;

    // 1. Flush stdout to avoid lingering buffered bytes being sent down the wrong stream
    let _ = io::stdout().flush();

    let stdout_fd = io::stdout().as_raw_fd();
    let stderr_fd = io::stderr().as_raw_fd();

    // 2. Duplicate the current stdout fd
    let saved_stdout = unsafe { libc::dup(stdout_fd) };
    if saved_stdout < 0 {
        return Err(anyhow::anyhow!(
            "Failed to duplicate stdout: {}",
            io::Error::last_os_error()
        ));
    }

    // 3. Redirect stdout to stderr
    let dup2_res = unsafe { libc::dup2(stderr_fd, stdout_fd) };
    if dup2_res < 0 {
        let err = io::Error::last_os_error();
        unsafe {
            libc::close(saved_stdout);
        }
        return Err(anyhow::anyhow!(
            "Failed to redirect stdout to stderr: {}",
            err
        ));
    }

    // 4. Run the requested CLI command
    let result = cmdhub_cli::runner::run_command(conn, cmd_path, args, true);

    // 5. Restore original stdout fd
    let restore_res = unsafe { libc::dup2(saved_stdout, stdout_fd) };
    unsafe {
        libc::close(saved_stdout);
    }

    if restore_res < 0 {
        return Err(anyhow::anyhow!(
            "Failed to restore stdout: {}",
            io::Error::last_os_error()
        ));
    }

    result
}

#[cfg(not(unix))]
fn run_command_safely(conn: &rusqlite::Connection, cmd_path: &str, args: &[String]) -> Result<()> {
    cmdhub_cli::runner::run_command(conn, cmd_path, args, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    #[cfg(unix)]
    fn capture_stdout<F: FnOnce()>(f: F) -> String {
        use std::io::Read;
        use std::os::unix::io::AsRawFd;

        let _ = io::stdout().flush();
        let stdout_fd = io::stdout().as_raw_fd();

        let saved_stdout = unsafe { libc::dup(stdout_fd) };
        assert!(saved_stdout >= 0);

        let mut pipe_fds = [0; 2];
        let pipe_res = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert!(pipe_res >= 0);

        let dup2_res = unsafe { libc::dup2(pipe_fds[1], stdout_fd) };
        assert!(dup2_res >= 0);

        f();

        let _ = io::stdout().flush();

        let restore_res = unsafe { libc::dup2(saved_stdout, stdout_fd) };
        assert!(restore_res >= 0);

        unsafe {
            libc::close(saved_stdout);
            libc::close(pipe_fds[1]);
        }

        let mut captured = String::new();
        let mut pipe_read = unsafe { std::fs::File::from_raw_fd(pipe_fds[0]) };
        let _ = pipe_read.read_to_string(&mut captured);

        captured
    }

    fn get_test_state() -> AppState {
        AppState {
            model: None,
            tokenizer: cmdhub_cli::tokenizer::Tokenizer::new(),
            config: cmdhub_cli::config::Config::default(),
        }
    }

    #[test]
    fn test_jsonrpc_parse_error() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let state = get_test_state();
        let bad_json = "{invalid}";
        #[cfg(unix)]
        {
            let output = capture_stdout(|| {
                let _ = handle_json_rpc_message(bad_json, &state);
            });
            assert!(output.contains("-32700"));
            assert!(output.contains("Parse error"));
        }
        #[cfg(not(unix))]
        {
            let res = handle_json_rpc_message(bad_json, &state);
            assert!(res.is_ok());
        }
    }

    #[test]
    fn test_jsonrpc_invalid_method() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let state = get_test_state();
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "non_existent_method"
        });
        if let Ok(line) = serde_json::to_string(&req) {
            #[cfg(unix)]
            {
                let output = capture_stdout(|| {
                    let _ = handle_json_rpc_message(&line, &state);
                });
                assert!(output.contains("-32601"));
                assert!(output.contains("Method not found"));
            }
            #[cfg(not(unix))]
            {
                let res = handle_json_rpc_message(&line, &state);
                assert!(res.is_ok());
            }
        }
    }

    #[test]
    fn test_mcp_initialize() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let state = get_test_state();
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 100,
            "method": "initialize"
        });
        let line = serde_json::to_string(&req).unwrap();
        #[cfg(unix)]
        {
            let output = capture_stdout(|| {
                let _ = handle_json_rpc_message(&line, &state);
            });
            assert!(output.contains("protocolVersion"));
            assert!(output.contains("cmdhub-mcp"));
            assert!(output.contains("100"));
        }
    }

    #[test]
    fn test_mcp_tools_list() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let state = get_test_state();
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 101,
            "method": "tools/list"
        });
        let line = serde_json::to_string(&req).unwrap();
        #[cfg(unix)]
        {
            let output = capture_stdout(|| {
                let _ = handle_json_rpc_message(&line, &state);
            });
            assert!(output.contains("cmdhub_search"));
            assert!(output.contains("cmdhub_execute"));
            assert!(output.contains("101"));
        }
    }

    #[test]
    fn test_mcp_tools_call_search() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().to_path_buf();

        std::env::set_var("XDG_CONFIG_HOME", &config_dir);
        std::env::set_var("XDG_DATA_HOME", &config_dir);

        // Seed DB with some values
        let conn = cmdhub_cli::db::open_db().unwrap();
        cmdhub_cli::db::init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
            ("org.test.mcp", "mcp-test", None::<String>),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                "mcp-test.hello",
                "org.test.mcp",
                "hello",
                "arg",
                "Say hello to the world from MCP test",
                "safe",
                "mcp-test hello",
            ),
        ).unwrap();

        conn.execute(
            "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
            (
                "mcp-test.hello",
                "mcp-test",
                "Say hello to the world from MCP test",
            ),
        )
        .unwrap();

        let state = get_test_state();
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 102,
            "method": "tools/call",
            "params": {
                "name": "cmdhub_search",
                "arguments": {
                    "query": "world",
                    "limit": 5
                }
            }
        });
        let line = serde_json::to_string(&req).unwrap();

        #[cfg(unix)]
        {
            let output = capture_stdout(|| {
                let _ = handle_json_rpc_message(&line, &state);
            });
            assert!(output.contains("mcp-test.hello"));
            assert!(output.contains("Say hello to the world"));
            assert!(output.contains("102"));
        }

        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_DATA_HOME");
    }

    #[test]
    fn test_mcp_tools_call_execute_safe() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let state = get_test_state();
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().to_path_buf();

        std::env::set_var("XDG_CONFIG_HOME", &config_dir);
        std::env::set_var("XDG_DATA_HOME", &config_dir);

        let conn = cmdhub_cli::db::open_db().unwrap();
        cmdhub_cli::db::init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
            ("org.test.echo", "echo", None::<String>),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                "echo.test",
                "org.test.echo",
                "test",
                "arg",
                "Echo test",
                "safe",
                "echo hello",
            ),
        ).unwrap();

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 103,
            "method": "tools/call",
            "params": {
                "name": "cmdhub_execute",
                "arguments": {
                    "cmd_path": "echo.test",
                    "args": ["hello_mcp_execute"]
                }
            }
        });
        let line = serde_json::to_string(&req).unwrap();

        #[cfg(unix)]
        {
            let output = capture_stdout(|| {
                let _ = handle_json_rpc_message(&line, &state);
            });
            assert!(output.contains("Command executed successfully"));
            assert!(output.contains("103"));
        }

        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_DATA_HOME");
    }

    #[test]
    #[cfg(unix)]
    fn test_run_command_safely_redirection() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().to_path_buf();

        std::env::set_var("XDG_CONFIG_HOME", &config_dir);
        std::env::set_var("XDG_DATA_HOME", &config_dir);

        let conn = cmdhub_cli::db::open_db().unwrap();
        cmdhub_cli::db::init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
            ("org.test.echo", "echo", None::<String>),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                "echo.test",
                "org.test.echo",
                "test",
                "arg",
                "Echo test",
                "safe",
                "echo hello",
            ),
        ).unwrap();

        // When we run echo.test through run_command_safely, standard output from echo is redirected to stderr.
        // Thus, capturing stdout should yield absolutely nothing, while executing correctly!
        let output = capture_stdout(|| {
            let res =
                run_command_safely(&conn, "echo.test", &["hello_mcp_redirection".to_string()]);
            assert!(res.is_ok());
        });

        // The hijacked stdout stream must be completely empty of command execution artifacts
        assert!(output.is_empty());

        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_DATA_HOME");
    }
}
