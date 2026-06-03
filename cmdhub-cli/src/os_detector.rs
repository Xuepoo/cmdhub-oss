use std::path::Path;

pub fn detect_os() -> Option<String> {
    if std::env::consts::OS == "macos" {
        return Some("macos".to_string());
    }
    parse_os_release(Path::new("/etc/os-release"))
}

fn parse_os_release(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut id = None;
    let mut id_like = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("ID=") {
            id = Some(strip_quotes(val));
        } else if let Some(val) = trimmed.strip_prefix("ID_LIKE=") {
            id_like = Some(strip_quotes(val));
        }
    }

    if let Some(ref val) = id {
        if is_recognized_os(val) {
            return Some(val.clone());
        }
    }

    if let Some(ref val) = id_like {
        for word in val.split_whitespace() {
            if is_recognized_os(word) {
                return Some(word.to_string());
            }
        }
    }
    id
}

fn strip_quotes(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '\'').to_string()
}

fn is_recognized_os(s: &str) -> bool {
    matches!(
        s,
        "macos"
            | "arch"
            | "ubuntu"
            | "debian"
            | "fedora"
            | "centos"
            | "rhel"
            | "gentoo"
            | "alpine"
            | "opensuse"
            | "nixos"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_os_release_parsing() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp.as_file(),
            "NAME=\"Ubuntu\"\nID=\"ubuntu\"\nID_LIKE=\"debian\"\n"
        )
        .unwrap();
        assert_eq!(parse_os_release(tmp.path()), Some("ubuntu".to_string()));

        let tmp_mint = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp_mint.as_file(),
            "NAME=\"Linux Mint\"\nID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"
        )
        .unwrap();
        assert_eq!(
            parse_os_release(tmp_mint.path()),
            Some("ubuntu".to_string())
        );
    }
}
