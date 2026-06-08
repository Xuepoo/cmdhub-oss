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
    pub status: String,
    pub install_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

pub fn resolve_install_command(contract: &AciCommandContract, config: &Config) -> Option<String> {
    let instructions = contract.install_instructions.as_ref()?;

    let os = config.install.os.clone().or_else(detect_os);

    // Build ordered candidate list:
    //   1. System package manager for the detected OS
    //   2. OS-specific AUR helpers (arch: yay, paru)
    //   3. User-configured package managers from config
    let mut candidates: Vec<(&str, bool)> = Vec::new();

    if let Some(ref os_str) = os {
        let sys_pm = map_os_to_package_manager(os_str);
        candidates.push((sys_pm, is_system_installer(sys_pm)));

        // Arch Linux: fall back to common AUR helpers when pacman doesn't have the package
        if os_str == "arch" {
            candidates.push(("yay", false));
            candidates.push(("paru", false));
        }
    }

    for pm in &config.install.package_managers {
        candidates.push((pm.as_str(), is_system_installer(pm)));
    }

    for (pm, is_sys) in candidates {
        if let Some(cmd) = instructions.get_command(pm) {
            return Some(if is_sys && !is_root_user() {
                format!("sudo {}", cmd)
            } else {
                cmd.clone()
            });
        }
    }

    None
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
        "windows" => "scoop",
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

pub fn resolve_binary_name(contract: &AciCommandContract, config: &Config) -> String {
    let os = config.install.os.clone().or_else(detect_os);
    let os_str = os.as_deref().unwrap_or("linux");

    if let Some(ref aliases) = contract.os_aliases {
        match os_str {
            "windows" => aliases
                .windows
                .clone()
                .unwrap_or_else(|| contract.name.clone()),
            "macos" => aliases
                .macos
                .clone()
                .unwrap_or_else(|| contract.name.clone()),
            _ => {
                // Linux or other unix-like
                if let Some(ref linux_aliases) = aliases.linux {
                    match linux_aliases {
                        cmdhub_shared::StringOrArray::Single(s) => s.clone(),
                        cmdhub_shared::StringOrArray::Multiple(arr) => {
                            if arr.is_empty() {
                                contract.name.clone()
                            } else {
                                for entry in arr {
                                    if which::which(entry).is_ok() {
                                        return entry.clone();
                                    }
                                }
                                arr[0].clone()
                            }
                        }
                    }
                } else {
                    contract.name.clone()
                }
            }
        }
    } else {
        contract.name.clone()
    }
}

pub fn check_is_installed(contract: &AciCommandContract, config: &Config) -> bool {
    let binary_name = resolve_binary_name(contract, config);
    which::which(&binary_name).is_ok()
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
                    let is_installed = check_is_installed(&c, config);
                    let status = if is_installed {
                        "installed".to_string()
                    } else {
                        "not_installed".to_string()
                    };
                    FullDto {
                        app_id: c.app_id,
                        name: c.name,
                        cmd_path: c.cmd_path,
                        node_type: format!("{:?}", c.node_type).to_lowercase(),
                        description: c.description,
                        risk_level: format!("{:?}", c.risk_level).to_lowercase(),
                        example_template: c.example_template,
                        status,
                        install_command,
                        docker_image: c.docker_image,
                        script_url: c.script_url,
                        source_url: c.source_url,
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
            os_aliases: None,
            install_instructions: Some(InstallInstructions {
                brew: Some("brew install test".to_string()),
                apt: Some("apt-get install test".to_string()),
                pacman: None,
                cargo: None,
                scoop: Some("scoop install test".to_string()),
                ..Default::default()
            }),
            docker_image: None,
            script_url: None,
            source_url: None,
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

    #[test]
    fn test_resolve_install_command_arch_aur_fallback() {
        use std::collections::HashMap;

        // Package only has yay/paru, not pacman
        let mut others = HashMap::new();
        others.insert("yay".to_string(), "yay -S python-pytube".to_string());
        others.insert("paru".to_string(), "paru -S python-pytube".to_string());

        let contract = AciCommandContract {
            app_id: "test".to_string(),
            name: "python-pytube".to_string(),
            cmd_path: "pytube".to_string(),
            node_type: cmdhub_shared::NodeType::Root,
            description: "test".to_string(),
            risk_level: cmdhub_shared::RiskLevel::Safe,
            example_template: None,
            os_aliases: None,
            install_instructions: Some(InstallInstructions {
                others,
                ..Default::default()
            }),
            docker_image: None,
            script_url: None,
            source_url: None,
        };

        let mut config = Config::default();
        config.install.os = Some("arch".to_string());

        // On arch, should fall back to yay when pacman not present
        let cmd = resolve_install_command(&contract, &config);
        assert_eq!(cmd, Some("yay -S python-pytube".to_string()));
    }
}
