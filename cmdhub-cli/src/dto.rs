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

    if let Some(installer) = sys_installer {
        if let Some(cmd) = instructions.get_command(installer) {
            return Some(cmd.clone());
        }
    }

    for pm in &config.install.package_managers {
        if let Some(cmd) = instructions.get_command(pm) {
            return Some(cmd.clone());
        }
    }

    None
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
