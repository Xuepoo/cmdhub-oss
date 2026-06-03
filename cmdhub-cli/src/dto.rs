use crate::config::Config;
use crate::os_detector::detect_os;
use cmdhub_shared::AciCommandContract;
use serde::Serialize;

#[derive(Serialize)]
pub struct UsageDto {
    pub cmd_path: String,
    pub example_template: Option<String>,
}

#[derive(Serialize)]
pub struct MinimalDto {
    pub cmd_path: String,
}

#[derive(Serialize)]
pub struct FullDto {
    pub app_id: String,
    pub name: String,
    pub cmd_path: String,
    pub node_type: String,
    pub description: String,
    pub risk_level: String,
    pub example_template: Option<String>,
    pub install_command: Option<String>,
}

pub fn resolve_install_command(contract: &AciCommandContract, config: &Config) -> Option<String> {
    let instructions = contract.install_instructions.as_ref()?;

    let os = config.install.os.clone().or_else(detect_os);
    let sys_installer = os.as_ref().map(|o| map_os_to_package_manager(o));

    let mut resolved = None;
    if let Some(installer) = sys_installer {
        if let Some(cmd) = instructions.get_command(installer) {
            resolved = Some((cmd.clone(), is_system_installer(installer)));
        }
    }

    if resolved.is_none() {
        for pm in &config.install.package_managers {
            if let Some(cmd) = instructions.get_command(pm) {
                resolved = Some((cmd.clone(), is_system_installer(pm)));
                break;
            }
        }
    }

    if let Some((cmd, is_sys)) = resolved {
        if is_sys && !is_root_user() {
            Some(format!("sudo {}", cmd))
        } else {
            Some(cmd)
        }
    } else {
        None
    }
}

#[cfg(unix)]
fn is_root_user() -> bool {
    unsafe { libc::getuid() == 0 }
}

#[cfg(not(unix))]
fn is_root_user() -> bool {
    false
}

fn is_system_installer(installer: &str) -> bool {
    matches!(
        installer,
        "apt" | "pacman" | "dnf" | "apk" | "emerge" | "zypper" | "yum"
    )
}

fn map_os_to_package_manager(os: &str) -> &str {
    match os {
        "macos" => "brew",
        "arch" => "pacman",
        "ubuntu" | "debian" => "apt",
        "fedora" => "dnf",
        "centos" | "rhel" => "yum",
        "gentoo" => "emerge",
        "alpine" => "apk",
        "opensuse" => "zypper",
        "nixos" => "nix-env",
        other => other,
    }
}

pub fn format_results(
    contracts: Vec<AciCommandContract>,
    mode: &str,
    config: &Config,
) -> serde_json::Value {
    match mode {
        "usage" => {
            let dtos: Vec<UsageDto> = contracts
                .into_iter()
                .map(|c| UsageDto {
                    cmd_path: c.cmd_path,
                    example_template: c.example_template,
                })
                .collect();
            serde_json::to_value(&dtos).unwrap()
        }
        "minimal" => {
            let dtos: Vec<MinimalDto> = contracts
                .into_iter()
                .map(|c| MinimalDto {
                    cmd_path: c.cmd_path,
                })
                .collect();
            serde_json::to_value(&dtos).unwrap()
        }
        _ => {
            let dtos: Vec<FullDto> = contracts
                .into_iter()
                .map(|c| {
                    let install_command = resolve_install_command(&c, config);
                    FullDto {
                        app_id: c.app_id,
                        name: c.name,
                        cmd_path: c.cmd_path,
                        node_type: format!("{:?}", c.node_type).to_lowercase(),
                        description: c.description,
                        risk_level: format!("{:?}", c.risk_level).to_lowercase(),
                        example_template: c.example_template,
                        install_command,
                    }
                })
                .collect();
            serde_json::to_value(&dtos).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cmdhub_shared::InstallInstructions;

    #[test]
    fn test_resolve_install_command_sudo() {
        let contract = AciCommandContract {
            app_id: "test".to_string(),
            name: "test".to_string(),
            cmd_path: "test".to_string(),
            node_type: cmdhub_shared::NodeType::Root,
            description: "test".to_string(),
            risk_level: cmdhub_shared::RiskLevel::Safe,
            example_template: None,
            install_instructions: Some(InstallInstructions {
                brew: Some("brew install test".to_string()),
                apt: Some("apt-get install test".to_string()),
                pacman: None,
                cargo: None,
                others: std::collections::HashMap::new(),
            }),
        };

        let mut config = Config::default();
        config.install.os = Some("debian".to_string());

        let cmd = resolve_install_command(&contract, &config).unwrap();
        if is_root_user() {
            assert_eq!(cmd, "apt-get install test");
        } else {
            assert_eq!(cmd, "sudo apt-get install test");
        }

        config.install.os = Some("macos".to_string());
        let cmd_brew = resolve_install_command(&contract, &config).unwrap();
        assert_eq!(cmd_brew, "brew install test");
    }
}
