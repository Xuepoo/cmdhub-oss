use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use tempfile::TempDir;

static TEST_MUTEX: Mutex<()> = Mutex::new(());

struct McpClient {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl McpClient {
    fn spawn(temp_dir: &std::path::Path) -> Self {
        // Find the cmdhub-mcp binary using assert_cmd
        let bin_path = assert_cmd::cargo::cargo_bin("cmdhub-mcp");
        let mut cmd = Command::new(bin_path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("XDG_CONFIG_HOME", temp_dir)
            .env("XDG_DATA_HOME", temp_dir);
        let mut child = cmd.spawn().expect("Failed to spawn cmdhub-mcp");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let reader = BufReader::new(stdout);
        Self { child, reader }
    }

    fn send_request(&mut self, req: &serde_json::Value) {
        let line = serde_json::to_string(req).unwrap();
        let stdin = self.child.stdin.as_mut().expect("Failed to open stdin");
        writeln!(stdin, "{}", line).unwrap();
        stdin.flush().unwrap();
    }

    fn recv_response(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .expect("Failed to read line");
        assert!(
            !line.is_empty(),
            "Received EOF instead of JSON-RPC response"
        );
        serde_json::from_str(&line).expect("Failed to parse JSON-RPC response")
    }
}

#[test]
fn test_mcp_integration_initialize_and_tools() {
    let _guard = TEST_MUTEX.lock().unwrap();
    let temp = TempDir::new().unwrap();
    let temp_path = temp.path().to_path_buf();

    // Set custom XDG environment
    std::env::set_var("XDG_CONFIG_HOME", &temp_path);
    std::env::set_var("XDG_DATA_HOME", &temp_path);

    // Initialise DB
    let conn = cmdhub_cli::db::open_db().unwrap();
    cmdhub_cli::db::init_db(&conn).unwrap();

    // Seed mock data for testing
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
            "Echo test from MCP integration test",
            "safe",
            "echo hello",
        ),
    ).unwrap();

    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
        ("echo.test", "echo", "Echo test from MCP integration test"),
    )
    .unwrap();

    // Spawn the MCP server child process
    let mut client = McpClient::spawn(&temp_path);

    // Scenario 1: initialize
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "test-client",
                "version": "1.0.0"
            }
        }
    });
    client.send_request(&init_req);
    let init_resp = client.recv_response();
    assert_eq!(init_resp["id"], 1);
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "cmdhub-mcp");

    // Scenario 2: tools/list
    let list_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    client.send_request(&list_req);
    let list_resp = client.recv_response();
    assert_eq!(list_resp["id"], 2);
    let tools = list_resp["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "cmdhub_search"));
    assert!(tools.iter().any(|t| t["name"] == "cmdhub_execute"));

    // Scenario 3: tools/call cmdhub_search
    let search_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "cmdhub_search",
            "arguments": {
                "query": "echo",
                "limit": 5
            }
        }
    });
    client.send_request(&search_req);
    let search_resp = client.recv_response();
    assert_eq!(search_resp["id"], 3);
    let content = search_resp["result"]["content"].as_array().unwrap();
    let text = content[0]["text"].as_str().unwrap();
    assert!(text.contains("echo.test"));

    // Scenario 4: tools/call cmdhub_execute
    // Executing "echo.test" with stdout redirection.
    // The redirected output should go to stderr, leaving stdout clean of non-JSON responses.
    let exec_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "cmdhub_execute",
            "arguments": {
                "cmd_path": "echo.test",
                "args": ["hello_mcp_redirection"]
            }
        }
    });
    client.send_request(&exec_req);
    let exec_resp = client.recv_response();
    assert_eq!(exec_resp["id"], 4);
    let exec_content = exec_resp["result"]["content"].as_array().unwrap();
    let exec_text = exec_content[0]["text"].as_str().unwrap();
    assert!(exec_text.contains("Command executed successfully"));

    // Clean up env
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_DATA_HOME");
}
