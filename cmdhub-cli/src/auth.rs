use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub token: String,
    pub expires_at: i64,
}

pub fn get_session_path() -> PathBuf {
    crate::config::get_config_dir().join("session.json")
}

pub fn get_session() -> Result<Option<Session>> {
    let path = get_session_path();
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).context("Failed to read session file")?;
    let session: Session =
        serde_json::from_str(&content).context("Failed to parse session file")?;
    Ok(Some(session))
}

pub async fn login_flow(config: &Config) -> Result<()> {
    eprintln!("Initiating login flow...");

    // 1. Generate PKCE verifier and challenge
    let code_verifier = Uuid::new_v4().to_string() + &Uuid::new_v4().to_string(); // high entropy
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let hash = hasher.finalize();

    use base64::Engine;
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash);
    let state = Uuid::new_v4().to_string();

    // 2. Start local TCP Listener
    let listener = tokio::net::TcpListener::bind("127.0.0.1:38118")
        .await
        .context(
            "Failed to bind local callback port 38118. Please check if it's already in use.",
        )?;

    let login_url = format!(
        "{}/auth/login/github?code_challenge={}&state={}",
        config.api_url, code_challenge, state
    );

    eprintln!("\nOpening browser for GitHub authentication...");
    eprintln!("If browser does not open automatically, please visit this URL:\n");
    eprintln!("👉 {}\n", login_url);

    // Try to open browser
    if !open_browser(&login_url) {
        eprintln!("Browser launcher not detected. Waiting for manual authentication callback...");
    }

    // 3. Receive OAuth callback via local web server
    let timeout_dur = std::time::Duration::from_secs(60);
    let mut code_and_state = None;

    match tokio::time::timeout(timeout_dur, async {
        loop {
            if let Ok((mut socket, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                if let Ok(n) = socket.read(&mut buf).await {
                    let req_str = String::from_utf8_lossy(&buf[..n]);
                    if let Some(first_line) = req_str.lines().next() {
                        if first_line.starts_with("GET /callback") {
                            if let Some(params_start) = first_line.find('?') {
                                let params_end = first_line[params_start..].find(' ').unwrap_or(first_line.len() - params_start) + params_start;
                                let query_str = &first_line[params_start + 1 .. params_end];
                                let mut code = None;
                                let mut oauth_state = None;
                                for part in query_str.split('&') {
                                    let mut kv = part.split('=');
                                    let key = kv.next();
                                    let val = kv.next();
                                    if let (Some(k), Some(v)) = (key, val) {
                                        if k == "code" {
                                            code = Some(v.to_string());
                                        } else if k == "state" {
                                            oauth_state = Some(v.to_string());
                                        }
                                    }
                                }
                                if let (Some(c), Some(s)) = (code, oauth_state) {
                                    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
                                                    <html><head><title>CmdHub Login Success</title><style>body { font-family: sans-serif; text-align: center; margin-top: 10%; background: #0f172a; color: #f1f5f9; } h1 { color: #38bdf8; }</style></head>\
                                                    <body><h1>Login Success!</h1><p>You can now close this browser tab and return to your terminal.</p></body></html>";
                                    let _ = socket.write_all(response.as_bytes()).await;
                                    let _ = socket.flush().await;
                                    return Some((c, s));
                                }
                            }
                        }
                    }
                    let response = "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\nInvalid callback request.";
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                }
            }
        }
    }).await {
        Ok(Some((c, s))) => {
            code_and_state = Some((c, s));
        }
        _ => {
            eprintln!("Authentication timed out (60 seconds). Please try again.");
        }
    }

    let (code, callback_state) =
        code_and_state.context("Authentication callback failed or timed out.")?;
    if callback_state != state {
        anyhow::bail!("Security Warning: State mismatch (request state: {}, callback state: {}). Potential CSRF attack blocked.", state, callback_state);
    }

    // 4. Exchange code for JWT
    eprintln!("Exchanging authentication code for API access token...");
    let client = Client::new();
    let token_res = client
        .post(format!("{}/auth/token", config.api_url))
        .json(&serde_json::json!({
            "code": code,
            "state": state,
            "code_verifier": code_verifier
        }))
        .send()
        .await
        .context("Failed to contact auth token endpoint")?;

    if !token_res.status().is_success() {
        let err_body = token_res.text().await.unwrap_or_default();
        anyhow::bail!("Cloud authentication failed: {}", err_body);
    }

    #[derive(serde::Deserialize)]
    struct TokenResponse {
        token: String,
        expires_in: usize,
    }

    let token_payload: TokenResponse = token_res
        .json()
        .await
        .context("Failed to parse token payload")?;

    // 5. Store session securely (0600 file permission)
    let session = Session {
        token: token_payload.token,
        expires_at: chrono::Utc::now().timestamp() + token_payload.expires_in as i64,
    };

    let session_path = get_session_path();
    let parent_dir = session_path.parent().unwrap();
    fs::create_dir_all(parent_dir)?;

    fs::write(&session_path, serde_json::to_string_pretty(&session)?)
        .context("Failed to save credentials session file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&session_path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&session_path, perms)
            .context("Failed to restrict session file permission (0600)")?;
    }

    println!("Welcome! You have successfully authenticated with CmdHub Cloud Registry.");
    Ok(())
}

pub async fn logout_flow(config: &Config) -> Result<()> {
    let session_opt = get_session()?;
    if let Some(session) = session_opt {
        eprintln!("Logging out from Cloud Registry...");

        let client = Client::new();
        let _ = client
            .post(format!("{}/auth/logout", config.api_url))
            .header("Authorization", format!("Bearer {}", session.token))
            .send()
            .await;

        let path = get_session_path();
        if path.exists() {
            fs::remove_file(&path).context("Failed to delete local session file")?;
        }
        println!("Successfully logged out and cleared local credentials.");
    } else {
        println!("You are not currently logged in.");
    }
    Ok(())
}

fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open").arg(url).status();
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .status();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let status = std::process::Command::new("xdg-open").arg(url).status();

    status.map(|s| s.success()).unwrap_or(false)
}
