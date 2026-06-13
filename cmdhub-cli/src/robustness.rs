const FAMOUS_COMMANDS: &[&str] = &[
    "mkdir",
    "rmdir",
    "chmod",
    "chown",
    "grep",
    "unzip",
    "curl",
    "ping",
    "clear",
    "terminal",
    "history",
    "systemctl",
    "journalctl",
    "uptime",
    "whoami",
    "touch",
    "head",
    "tail",
    "kill",
    "pkill",
    "alias",
];

#[allow(clippy::needless_range_loop)]
fn levenshtein_distance(s1: &str, s2: &str) -> usize {
    let len1 = s1.chars().count();
    let len2 = s2.chars().count();
    if len1 == 0 {
        return len2;
    }
    if len2 == 0 {
        return len1;
    }

    let mut dp = vec![vec![0; len2 + 1]; len1 + 1];
    for i in 0..=len1 {
        dp[i][0] = i;
    }
    for j in 0..=len2 {
        dp[0][j] = j;
    }

    for (i, c1) in s1.chars().enumerate() {
        for (j, c2) in s2.chars().enumerate() {
            let cost = if c1 == c2 { 0 } else { 1 };
            dp[i + 1][j + 1] = std::cmp::min(
                std::cmp::min(dp[i][j + 1] + 1, dp[i + 1][j] + 1),
                dp[i][j] + cost,
            );
        }
    }
    dp[len1][len2]
}

fn correct_typo(token: &str) -> String {
    // 1. Precise mapping for high-frequency short command typos
    let exact_mapping = [
        ("makdir", "mkdir"),
        ("rmdier", "rmdir"),
        ("chomd", "chmod"),
        ("chownn", "chown"),
        ("gerp", "grep"),
        ("unzipp", "unzip"),
        ("curll", "curl"),
        ("pign", "ping"),
        ("rerminal", "terminal"),
        ("psx", "ps"),
        ("hstry", "history"),
        ("lss", "ls"),
        ("pwdd", "pwd"),
        ("cdd", "cd"),
        ("tars", "tar"),
        ("systmctl", "systemctl"),
        ("journlctl", "journalctl"),
        ("upime", "uptime"),
        ("whoamii", "whoami"),
        ("copiy", "cp"),
        ("mve", "mv"),
        ("toutch", "touch"),
        ("headd", "head"),
        ("taill", "tail"),
        ("dff", "df"),
        ("duu", "du"),
        ("kll", "kill"),
        ("pkll", "pkill"),
        ("aliass", "alias"),
    ];

    for &(typo, correction) in &exact_mapping {
        if token == typo {
            return correction.to_string();
        }
    }

    // 2. Generic Levenshtein correction for longer commands (length > 3)
    if token.len() <= 3 {
        return token.to_string();
    }

    let mut best_match = None;
    let mut min_dist = 2; // Threshold is 1 for most words

    let limit = if token.len() >= 8 { 2 } else { 1 };

    for &cmd in FAMOUS_COMMANDS {
        let dist = levenshtein_distance(token, cmd);
        if dist <= limit && dist < min_dist {
            min_dist = dist;
            best_match = Some(cmd);
        }
    }

    if let Some(cmd) = best_match {
        cmd.to_string()
    } else {
        token.to_string()
    }
}

fn translate_pinyin_token(token: &str) -> String {
    let translation = match token {
        "shanchu" => "delete",
        "wenjian" => "file",
        "chaxun" => "search",
        "jincheng" => "process",
        "jiazai" => "mount",
        "cipan" => "disk",
        "kaishi" => "start",
        "jiancha" => "check",
        "neicun" => "memory",
        "qingchu" => "clear",
        "pingmu" => "screen",
        "liechu" => "list",
        "mulu" => "directory",
        "duibi" => "diff",
        "xiazai" => "download",
        "chuangjian" => "create",
        "fuzhi" => "copy",
        "yidong" => "move",
        "kong" => "empty",
        "xiugai" => "modify",
        "quanxian" => "permission",
        "suoyouzhi" => "owner",
        "sousuo" => "search",
        "ping" => "ping",
        "wangluo" => "network",
        "dakai" => "open",
        "yuancheng" => "remote",
        "yasuo" => "compress",
        "jieya" => "decompress",
        _ => token,
    };
    translation.to_string()
}

pub fn preprocess_robustness(query: &str) -> String {
    let mut current_word = String::new();
    let mut result = String::new();

    for c in query.chars() {
        if c.is_alphanumeric() {
            current_word.push(c);
        } else {
            if !current_word.is_empty() {
                let lower = current_word.to_lowercase();
                let pinyin_translation = translate_pinyin_token(&lower);
                let corrected = if pinyin_translation != lower {
                    pinyin_translation
                } else {
                    correct_typo(&lower)
                };

                if corrected == lower {
                    result.push_str(&current_word);
                } else {
                    result.push_str(&corrected);
                }
                current_word.clear();
            }
            result.push(c);
        }
    }

    if !current_word.is_empty() {
        let lower = current_word.to_lowercase();
        let pinyin_translation = translate_pinyin_token(&lower);
        let corrected = if pinyin_translation != lower {
            pinyin_translation
        } else {
            correct_typo(&lower)
        };

        if corrected == lower {
            result.push_str(&current_word);
        } else {
            result.push_str(&corrected);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein_distance("mkdir", "makdir"), 1);
        assert_eq!(levenshtein_distance("chmod", "chomd"), 2); // Transposition = 2 in standard Levenshtein
        assert_eq!(levenshtein_distance("journalctl", "journlctl"), 1);
    }

    #[test]
    fn test_robustness_preprocessing() {
        // Pinyin mapping
        assert_eq!(preprocess_robustness("shanchu wenjian"), "delete file");
        assert_eq!(
            preprocess_robustness("kaishi systemd jincheng"),
            "start systemd process"
        );

        // Typo correction
        assert_eq!(
            preprocess_robustness("makdir test_folder"),
            "mkdir test_folder"
        );
        assert_eq!(preprocess_robustness("cdd .."), "cd ..");
        assert_eq!(
            preprocess_robustness("systmctl status nginx"),
            "systemctl status nginx"
        );
    }
}
