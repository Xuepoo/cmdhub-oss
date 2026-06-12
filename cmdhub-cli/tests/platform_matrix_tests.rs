use assert_cmd::Command;
use cmdhub_cli::config::Config;
use cmdhub_cli::dto::{resolve_binary_name, resolve_install_command};
use cmdhub_shared::{
    AciCommandContract, InstallInstructions, NodeType, OsAliases, RiskLevel, StringOrArray,
};
use tempfile::TempDir;

#[test]
fn test_platform_install_command_resolution() {
    let contract = AciCommandContract {
        app_id: "org.test.platform".to_string(),
        name: "mytool".to_string(),
        cmd_path: "mytool.run".to_string(),
        node_type: NodeType::Root,
        description: "platform matrix test".to_string(),
        risk_level: RiskLevel::Safe,
        example_template: None,
        os_aliases: None,
        install_instructions: Some(InstallInstructions {
            brew: Some("brew install mytool".to_string()),
            apt: Some("apt-get install mytool".to_string()),
            scoop: Some("scoop install mytool".to_string()),
            pacman: Some("pacman -S mytool".to_string()),
            ..Default::default()
        }),
        docker_image: None,
        script_url: None,
        source_url: None,
        popularity: 0.0,
        verified: false,
    };

    // 1. Ubuntu/Debian mapping (APT) with sudo check
    let mut config = Config::default();
    config.install.os = Some("ubuntu".to_string());
    let cmd_ubuntu = resolve_install_command(&contract, &config).unwrap();
    // APT is a system installer. On non-root UNIX it should prefix with 'sudo'.
    #[cfg(unix)]
    {
        let is_root = unsafe { libc::getuid() == 0 };
        if is_root {
            assert_eq!(cmd_ubuntu, "apt-get install mytool");
        } else {
            assert_eq!(cmd_ubuntu, "sudo apt-get install mytool");
        }
    }
    #[cfg(not(unix))]
    {
        assert_eq!(cmd_ubuntu, "apt-get install mytool");
    }

    // 2. macOS mapping (Brew) - should not prepend sudo
    config.install.os = Some("macos".to_string());
    let cmd_macos = resolve_install_command(&contract, &config).unwrap();
    assert_eq!(cmd_macos, "brew install mytool");

    // 3. Windows mapping (Scoop) - should not prepend sudo
    config.install.os = Some("windows".to_string());
    let cmd_windows = resolve_install_command(&contract, &config).unwrap();
    assert_eq!(cmd_windows, "scoop install mytool");

    // 4. Arch mapping (Pacman) with sudo check
    config.install.os = Some("arch".to_string());
    let cmd_arch = resolve_install_command(&contract, &config).unwrap();
    #[cfg(unix)]
    {
        let is_root = unsafe { libc::getuid() == 0 };
        if is_root {
            assert_eq!(cmd_arch, "pacman -S mytool");
        } else {
            assert_eq!(cmd_arch, "sudo pacman -S mytool");
        }
    }
    #[cfg(not(unix))]
    {
        assert_eq!(cmd_arch, "pacman -S mytool");
    }
}

#[test]
fn test_platform_os_aliases_probing() {
    // We mock a command contract that has multiple aliases for linux.
    // 'sh' is guaranteed to exist on modern Unix/Linux platforms, while 'nonexistentbinary123' does not.
    let contract = AciCommandContract {
        app_id: "org.test.aliases".to_string(),
        name: "custom_shell".to_string(),
        cmd_path: "custom_shell.run".to_string(),
        node_type: NodeType::Root,
        description: "os aliases test".to_string(),
        risk_level: RiskLevel::Safe,
        example_template: None,
        os_aliases: Some(OsAliases {
            windows: Some("custom_shell.exe".to_string()),
            macos: Some("custom_shell_mac".to_string()),
            linux: Some(StringOrArray::Multiple(vec![
                "nonexistentbinary123".to_string(),
                "sh".to_string(), // will resolve to this since it exists
                "another_nonexistent".to_string(),
            ])),
        }),
        install_instructions: None,
        docker_image: None,
        script_url: None,
        source_url: None,
        popularity: 0.0,
        verified: false,
    };

    let mut config = Config::default();

    // 1. Test Linux behavior
    config.install.os = Some("linux".to_string());
    let resolved_linux = resolve_binary_name(&contract, &config);
    #[cfg(unix)]
    {
        // 'sh' is expected to be found by `which` and resolved
        assert_eq!(resolved_linux, "sh");
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, it doesn't probe Unix paths successfully, fallback to the first entry
        assert_eq!(resolved_linux, "nonexistentbinary123");
    }

    // 2. Test macOS behavior
    config.install.os = Some("macos".to_string());
    let resolved_macos = resolve_binary_name(&contract, &config);
    assert_eq!(resolved_macos, "custom_shell_mac");

    // 3. Test Windows behavior
    config.install.os = Some("windows".to_string());
    let resolved_win = resolve_binary_name(&contract, &config);
    assert_eq!(resolved_win, "custom_shell.exe");
}

#[test]
fn test_cli_suggests_correct_install_command_via_config() {
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().to_path_buf();

    // Create a mock DB with a platform-specific package
    std::env::set_var("XDG_DATA_HOME", &data_dir);
    let conn = cmdhub_cli::db::open_db().unwrap();
    cmdhub_cli::db::init_db(&conn).unwrap();

    conn.execute(
        "INSERT INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
        (
            "org.test.cliplatform",
            "cliplatform",
            "{\"brew\": \"brew install cliplatform\", \"scoop\": \"scoop install cliplatform\", \"apt\": \"apt-get install cliplatform\"}",
        ),
    )
    .unwrap();

    conn.execute(
        "INSERT INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            "cliplatform",
            "org.test.cliplatform",
            "cliplatform",
            "root",
            "cli platform description",
            "safe",
            "cliplatform --help",
        ),
    ).unwrap();

    conn.execute(
        "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
        ("cliplatform", "cliplatform", "cli platform description"),
    )
    .unwrap();

    drop(conn);

    // 1. Mock Windows OS in config.toml
    let config_path = data_dir.join("cmdhub/config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "api_url = \"https://api.cmdhub.org\"\npublic_key = \"\"\ntimeout_seconds = 30\n[install]\nos = \"windows\"\n").unwrap();

    // Run CLI search
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("search")
        .arg("cliplatform")
        .arg("--full");
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = json.as_array().unwrap();
    assert_eq!(arr[0]["install_command"], "scoop install cliplatform");

    // 2. Mock macOS OS in config.toml
    std::fs::write(&config_path, "api_url = \"https://api.cmdhub.org\"\npublic_key = \"\"\ntimeout_seconds = 30\n[install]\nos = \"macos\"\n").unwrap();

    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &data_dir)
        .arg("search")
        .arg("cliplatform")
        .arg("--full");
    let assert = cmd.assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = json.as_array().unwrap();
    assert_eq!(arr[0]["install_command"], "brew install cliplatform");
}
