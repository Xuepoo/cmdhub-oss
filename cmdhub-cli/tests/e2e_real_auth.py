#!/usr/bin/env python3
import subprocess
import time
import os
import urllib.parse
import re
import requests
import shutil

def run_e2e_test():
    print("=== Starting Real Scenario E2E Auth Test ===")
    
    # 1. Setup temporary directory for XDG Config isolation
    tmp_config_dir = "/tmp/cmdhub_e2e_config"
    if os.path.exists(tmp_config_dir):
        shutil.rmtree(tmp_config_dir)
    os.makedirs(tmp_config_dir)
    
    # Configure Env
    env = os.environ.copy()
    env["XDG_CONFIG_HOME"] = tmp_config_dir
    env["DATABASE_URL"] = "postgres://cmdhub_user:password@localhost:5432/cmdhub"
    env["JWT_SECRET"] = "e2e-testing-secret-key-12345"
    env["CMDHub_HEADLESS"] = "1"
    
    # 2. Compile binaries to ensure fresh builds
    print("Compiling cloud-core and cmdh CLI...")
    # Get base workspace directory
    base_dir = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../../"))
    
    subprocess.run(["cargo", "build", "-p", "cloud-core"], cwd=os.path.join(base_dir, "cmdhub-cloud"), check=True)
    subprocess.run(["cargo", "build", "-p", "cmdhub-cli"], cwd=os.path.join(base_dir, "cmdhub-oss"), check=True)
    
    cloud_core_bin = os.path.join(base_dir, "cmdhub-cloud/target/debug/cloud-core")
    cmdh_bin = os.path.join(base_dir, "cmdhub-oss/target/debug/cmdh")
    
    # 3. Start cloud-core API in background
    print("Launching cloud-core API server...")
    api_proc = subprocess.Popen(
        [cloud_core_bin],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env
    )
    time.sleep(2)  # wait for API to bind port 8080 and run migrations
    
    # 4. Initialize config for CLI
    print("Initializing CLI config.toml...")
    subprocess.run([cmdh_bin, "init", "--force"], env=env, check=True)
    
    # Modify CLI config.toml to point to local cloud API
    config_path = os.path.join(tmp_config_dir, "cmdhub", "config.toml")
    with open(config_path, "r") as f:
        config_data = f.read()
    
    config_data = config_data.replace(
        'api_url = "https://api.cmdhub.xyz"',
        'api_url = "http://127.0.0.1:8080/api/v1"'
    )
    with open(config_path, "w") as f:
        f.write(config_data)

    try:
        # 5. Launch cmdh login in background
        print("Running 'cmdh login'...")
        cli_proc = subprocess.Popen(
            [cmdh_bin, "login"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env
        )
        
        # 6. Extract login URL from stderr
        time.sleep(1.5)
        stderr_output = ""
        os.set_blocking(cli_proc.stderr.fileno(), False)
        try:
            stderr_output = cli_proc.stderr.read() or ""
        except Exception:
            pass
        
        print(f"Captured CLI Stderr:\n{stderr_output}")
        
        url_match = re.search(r"👉 (http://\S+)", stderr_output)
        if not url_match:
            raise RuntimeError("Failed to capture login URL from CLI output.")
        
        login_url = url_match.group(1)
        print(f"Captured URL: {login_url}")
        
        # Parse query params to extract state
        parsed_url = urllib.parse.urlparse(login_url)
        query_params = urllib.parse.parse_qs(parsed_url.query)
        state = query_params.get("state", [None])[0]
        if not state:
            raise RuntimeError("Missing state parameter in login URL")
        
        # 6.5 Register PKCE challenge state on the cloud API
        print("Registering challenge state on the cloud API via login_url...")
        login_res = requests.get(login_url, allow_redirects=False)
        assert login_res.status_code == 307, f"Expected 307 redirect, got {login_res.status_code}"

        # 7. Perform mock GET callback request to local port 38118
        print(f"Simulating browser redirect to local listener with state: {state}...")
        callback_url = f"http://127.0.0.1:38118/callback?code=mock_code_123&state={state}"
        res = requests.get(callback_url, timeout=5)
        assert res.status_code == 200
        assert "Login Success" in res.text
        
        # Wait for CLI to finish token exchange
        cli_proc.wait(timeout=5)
        if cli_proc.returncode != 0:
            try:
                os.set_blocking(cli_proc.stderr.fileno(), False)
                extra_stderr = cli_proc.stderr.read() or ""
            except Exception:
                extra_stderr = "Could not read stderr"
            try:
                os.set_blocking(cli_proc.stdout.fileno(), False)
                extra_stdout = cli_proc.stdout.read() or ""
            except Exception:
                extra_stdout = "Could not read stdout"
            print(f"CLI exited with code {cli_proc.returncode}.\nStderr:\n{extra_stderr}\nStdout:\n{extra_stdout}")
        assert cli_proc.returncode == 0
        print("CLI process exited successfully.")
        
        # 8. Check if session.json is created and verify 0600 permissions
        session_path = os.path.join(tmp_config_dir, "cmdhub", "session.json")
        assert os.path.exists(session_path), "session.json was not created!"
        
        if os.name == "posix":
            perms = oct(os.stat(session_path).st_mode & 0o777)
            print(f"session.json file permissions: {perms}")
            assert perms == "0o600", "Security violation: session.json does not have 0600 permissions!"
            
        print("Authentication verified! session.json created securely.")
        
        # 9. Verify logout
        print("Running 'cmdh logout'...")
        logout_res = subprocess.run([cmdh_bin, "logout"], env=env, capture_output=True, text=True, check=True)
        assert "Successfully logged out" in logout_res.stdout
        assert not os.path.exists(session_path), "session.json was not deleted on logout!"
        print("Logout verified! credentials cleared safely.")
        
        print("\n🎉 ALL E2E AUTHENTICATION SCENARIOS PASSED SUCCESSFULLY!")
        
    finally:
        print("Cleaning up background servers...")
        api_proc.terminate()
        api_proc.wait()
        shutil.rmtree(tmp_config_dir, ignore_errors=True)

if __name__ == "__main__":
    run_e2e_test()
