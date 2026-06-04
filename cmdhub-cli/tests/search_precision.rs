use cmdhub_cli::db::{init_db, search_commands};
use cmdhub_cli::inference::EmbeddingModel;
use cmdhub_cli::tokenizer::Tokenizer;
use rusqlite::Connection;
use std::path::PathBuf;
use tempfile::TempDir;

struct TestCommand {
    name: &'static str,
    cmd_path: &'static str,
    description: &'static str,
}

struct GoldenCase {
    query: &'static str,
    target_cmd: &'static str,
}

// 50 standard Unix commands with distinct descriptions
static TEST_COMMANDS: &[TestCommand] = &[
    TestCommand {
        name: "ls",
        cmd_path: "ls",
        description: "List directory contents with details",
    },
    TestCommand {
        name: "pwd",
        cmd_path: "pwd",
        description: "Print name of current/working directory",
    },
    TestCommand {
        name: "cd",
        cmd_path: "cd",
        description: "Change the shell working directory",
    },
    TestCommand {
        name: "mkdir",
        cmd_path: "mkdir",
        description: "Create directory recursively if not exists",
    },
    TestCommand {
        name: "rmdir",
        cmd_path: "rmdir",
        description: "Remove empty directories",
    },
    TestCommand {
        name: "rm",
        cmd_path: "rm",
        description: "Remove files or directories recursively",
    },
    TestCommand {
        name: "cp",
        cmd_path: "cp",
        description: "Copy files and directories",
    },
    TestCommand {
        name: "mv",
        cmd_path: "mv",
        description: "Move or rename files and directories",
    },
    TestCommand {
        name: "touch",
        cmd_path: "touch",
        description: "Change file timestamps or create empty file",
    },
    TestCommand {
        name: "chmod",
        cmd_path: "chmod",
        description: "Change file mode bits / permissions",
    },
    TestCommand {
        name: "chown",
        cmd_path: "chown",
        description: "Change file owner and group",
    },
    TestCommand {
        name: "find",
        cmd_path: "find",
        description: "Search for files in a directory hierarchy by name",
    },
    TestCommand {
        name: "grep",
        cmd_path: "grep",
        description: "Print lines matching a pattern / regular expression",
    },
    TestCommand {
        name: "cat",
        cmd_path: "cat",
        description: "Concatenate files and print on the standard output",
    },
    TestCommand {
        name: "less",
        cmd_path: "less",
        description: "View file contents with backward and forward navigation",
    },
    TestCommand {
        name: "head",
        cmd_path: "head",
        description: "Output the first part of files",
    },
    TestCommand {
        name: "tail",
        cmd_path: "tail",
        description: "Output the last part of files",
    },
    TestCommand {
        name: "tar",
        cmd_path: "tar",
        description: "Write and extract tape archive files compressed",
    },
    TestCommand {
        name: "zip",
        cmd_path: "zip",
        description: "Package and compress files in zip format",
    },
    TestCommand {
        name: "unzip",
        cmd_path: "unzip",
        description: "Extract compressed files from a ZIP archive",
    },
    TestCommand {
        name: "df",
        cmd_path: "df",
        description: "Show file system disk space usage",
    },
    TestCommand {
        name: "du",
        cmd_path: "du",
        description: "Estimate file space usage of directories",
    },
    TestCommand {
        name: "free",
        cmd_path: "free",
        description: "Display amount of free and used memory in the system",
    },
    TestCommand {
        name: "top",
        cmd_path: "top",
        description: "Display Linux processes dynamically in real time",
    },
    TestCommand {
        name: "ps",
        cmd_path: "ps",
        description: "Report a snapshot of the current processes",
    },
    TestCommand {
        name: "kill",
        cmd_path: "kill",
        description: "Send a signal to a process by PID",
    },
    TestCommand {
        name: "pkill",
        cmd_path: "pkill",
        description: "Look up or signal processes based on name",
    },
    TestCommand {
        name: "ping",
        cmd_path: "ping",
        description: "Send ICMP ECHO_REQUEST packets to network hosts",
    },
    TestCommand {
        name: "curl",
        cmd_path: "curl",
        description: "Transfer data from or to a server using URL syntax",
    },
    TestCommand {
        name: "wget",
        cmd_path: "wget",
        description: "Non-interactive network downloader for files",
    },
    TestCommand {
        name: "ssh",
        cmd_path: "ssh",
        description: "Secure shell client for remote login",
    },
    TestCommand {
        name: "scp",
        cmd_path: "scp",
        description: "Secure copy file transfer over SSH",
    },
    TestCommand {
        name: "git",
        cmd_path: "git",
        description: "The stupid content tracker / distributed version control",
    },
    TestCommand {
        name: "docker",
        cmd_path: "docker",
        description: "Pack, ship and run applications as lightweight containers",
    },
    TestCommand {
        name: "systemctl",
        cmd_path: "systemctl",
        description: "Control the systemd system and service manager",
    },
    TestCommand {
        name: "journalctl",
        cmd_path: "journalctl",
        description: "Query the systemd journal logs",
    },
    TestCommand {
        name: "uname",
        cmd_path: "uname",
        description: "Print system information like kernel name",
    },
    TestCommand {
        name: "whoami",
        cmd_path: "whoami",
        description: "Print the effective username of current user",
    },
    TestCommand {
        name: "date",
        cmd_path: "date",
        description: "Print or set the system date and time",
    },
    TestCommand {
        name: "uptime",
        cmd_path: "uptime",
        description: "Tell how long the system has been running",
    },
    TestCommand {
        name: "history",
        cmd_path: "history",
        description: "Command history list of the shell",
    },
    TestCommand {
        name: "alias",
        cmd_path: "alias",
        description: "Define or display aliases for commands",
    },
    TestCommand {
        name: "clear",
        cmd_path: "clear",
        description: "Clear the terminal screen",
    },
    TestCommand {
        name: "diff",
        cmd_path: "diff",
        description: "Compare files line by line",
    },
    TestCommand {
        name: "ln",
        cmd_path: "ln",
        description: "Make links between files / symlink",
    },
    TestCommand {
        name: "chroot",
        cmd_path: "chroot",
        description: "Run command or shell with special root directory",
    },
    TestCommand {
        name: "mount",
        cmd_path: "mount",
        description: "Mount a filesystem to directory",
    },
    TestCommand {
        name: "umount",
        cmd_path: "umount",
        description: "Unmount filesystems",
    },
    TestCommand {
        name: "ip",
        cmd_path: "ip",
        description: "Show / manipulate routing, network devices, interfaces",
    },
];

// 50 high-frequency natural language search intents
static GOLDEN_CASES: &[GoldenCase] = &[
    GoldenCase {
        query: "Delete my local files recursively",
        target_cmd: "rm",
    },
    GoldenCase {
        query: "list directory files with detail",
        target_cmd: "ls",
    },
    GoldenCase {
        query: "print working directory path",
        target_cmd: "pwd",
    },
    GoldenCase {
        query: "change current folder directory",
        target_cmd: "cd",
    },
    GoldenCase {
        query: "make nested folders if not exist",
        target_cmd: "mkdir",
    },
    GoldenCase {
        query: "remove empty directories inside workspace",
        target_cmd: "rmdir",
    },
    GoldenCase {
        query: "copy documents to another location",
        target_cmd: "cp",
    },
    GoldenCase {
        query: "move text file to target folder",
        target_cmd: "mv",
    },
    GoldenCase {
        query: "create an empty text file",
        target_cmd: "touch",
    },
    GoldenCase {
        query: "modify mode permissions of file",
        target_cmd: "chmod",
    },
    GoldenCase {
        query: "change file owner group to root",
        target_cmd: "chown",
    },
    GoldenCase {
        query: "search files by matching filename pattern",
        target_cmd: "find",
    },
    GoldenCase {
        query: "print lines matching standard regex expression",
        target_cmd: "grep",
    },
    GoldenCase {
        query: "print text file contents out to stdout",
        target_cmd: "cat",
    },
    GoldenCase {
        query: "scroll through text file backward and forward",
        target_cmd: "less",
    },
    GoldenCase {
        query: "get first few lines of log file",
        target_cmd: "head",
    },
    GoldenCase {
        query: "tail the last part of error logs",
        target_cmd: "tail",
    },
    GoldenCase {
        query: "create compressed tape archive",
        target_cmd: "tar",
    },
    GoldenCase {
        query: "zip folder directory files",
        target_cmd: "zip",
    },
    GoldenCase {
        query: "unzip archive zip folder",
        target_cmd: "unzip",
    },
    GoldenCase {
        query: "check dynamic disk partition space usage",
        target_cmd: "df",
    },
    GoldenCase {
        query: "calculate directory storage disk usage space",
        target_cmd: "du",
    },
    GoldenCase {
        query: "display free memory RAM stats in system",
        target_cmd: "free",
    },
    GoldenCase {
        query: "monitor system processes dynamically live",
        target_cmd: "top",
    },
    GoldenCase {
        query: "print snapshot of all running processes in system",
        target_cmd: "ps",
    },
    GoldenCase {
        query: "kill process by pid identifier",
        target_cmd: "kill",
    },
    GoldenCase {
        query: "signal and lookup processes by executable name",
        target_cmd: "pkill",
    },
    GoldenCase {
        query: "ping server network host address",
        target_cmd: "ping",
    },
    GoldenCase {
        query: "download asset from website url API",
        target_cmd: "curl",
    },
    GoldenCase {
        query: "download file in background non-interactively",
        target_cmd: "wget",
    },
    GoldenCase {
        query: "login to remote SSH server client shell",
        target_cmd: "ssh",
    },
    GoldenCase {
        query: "securely copy documents over SSH network protocol",
        target_cmd: "scp",
    },
    GoldenCase {
        query: "commit files using version control git tool",
        target_cmd: "git",
    },
    GoldenCase {
        query: "run lightweight container image runtime docker",
        target_cmd: "docker",
    },
    GoldenCase {
        query: "start service manager systemctl service unit",
        target_cmd: "systemctl",
    },
    GoldenCase {
        query: "query journalctl logging history and daemon logs",
        target_cmd: "journalctl",
    },
    GoldenCase {
        query: "print kernel version name system config",
        target_cmd: "uname",
    },
    GoldenCase {
        query: "get active effective username of session",
        target_cmd: "whoami",
    },
    GoldenCase {
        query: "show current date system time specs",
        target_cmd: "date",
    },
    GoldenCase {
        query: "check how long server has been running uptime",
        target_cmd: "uptime",
    },
    GoldenCase {
        query: "retrieve history list of typed terminal commands",
        target_cmd: "history",
    },
    GoldenCase {
        query: "create shortcut custom commands alias alias",
        target_cmd: "alias",
    },
    GoldenCase {
        query: "clear shell window terminal screen view",
        target_cmd: "clear",
    },
    GoldenCase {
        query: "compare differences between files line by line",
        target_cmd: "diff",
    },
    GoldenCase {
        query: "create symbolic links symlink between documents",
        target_cmd: "ln",
    },
    GoldenCase {
        query: "change shell root directory using chroot tool",
        target_cmd: "chroot",
    },
    GoldenCase {
        query: "mount external storage device filesystem",
        target_cmd: "mount",
    },
    GoldenCase {
        query: "unmount hard drive partition partition mountpoint",
        target_cmd: "umount",
    },
    GoldenCase {
        query: "show active network devices interfaces ip links",
        target_cmd: "ip",
    },
    GoldenCase {
        query: "display routing configuration ip table entries",
        target_cmd: "ip",
    },
];

fn find_bge_model_path() -> PathBuf {
    // 1. Try XDG Data path
    let local_share = PathBuf::from("/home/fuyu/.local/share/cmdhub/models/bge-micro-v2.onnx");
    if local_share.exists() {
        return local_share;
    }
    // 2. Try default Cache path
    let cache_dir = PathBuf::from("/home/fuyu/.cache/cmdhub/models/bge-micro-v2.onnx");
    if cache_dir.exists() {
        return cache_dir;
    }
    // 3. Fallback to default resolve
    cmdhub_cli::config::get_cache_dir().join("models/bge-micro-v2.onnx")
}

#[tokio::test]
async fn test_hybrid_search_accuracy_precision_evaluation() {
    let model_path = find_bge_model_path();
    if !model_path.exists() {
        eprintln!(
            "Skipping search accuracy test: ONNX model does not exist at {:?}",
            model_path
        );
        return;
    }

    let tokenizer = Tokenizer::new();
    let model = EmbeddingModel::load(&model_path).unwrap();

    // Enable sqlite-vec extension auto-load BEFORE connection opens!
    unsafe {
        type SqliteVecInitFn = unsafe extern "C" fn();
        let init_fn: SqliteVecInitFn = sqlite_vec::sqlite3_vec_init;
        #[allow(clippy::missing_transmute_annotations)]
        let _ = rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(init_fn)));
    }

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("precision_test.db");
    let conn = Connection::open(&db_path).unwrap();
    init_db(&conn).unwrap();

    let _ = conn.execute(cmdhub_shared::CREATE_COMMANDS_VEC_TABLE, []);

    // 1. Seed commands into SQLite with real embeddings
    for cmd in TEST_COMMANDS {
        let app_id = format!("org.test.{}", cmd.name);
        conn.execute(
            "INSERT OR REPLACE INTO apps (app_id, name, install_instructions) VALUES (?1, ?2, ?3)",
            (&app_id, cmd.name, Some("{}".to_string())),
        )
        .unwrap();

        conn.execute(
            "INSERT OR REPLACE INTO arguments (cmd_path, app_id, node_name, node_type, description, risk_level, example_template) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                cmd.cmd_path,
                &app_id,
                cmd.name,
                "arg",
                cmd.description,
                "safe",
                Some(cmd.name.to_string()),
            ),
        )
        .unwrap();

        // Write virtual tables
        conn.execute(
            "INSERT INTO apps_fts (cmd_path, name, capabilities) VALUES (?1, ?2, ?3)",
            (cmd.cmd_path, cmd.name, cmd.description),
        )
        .unwrap();

        // Generate embedding
        let (ids, mask) = tokenizer.tokenize_passage(cmd.description);
        let embedding = model.generate_embedding(&ids, &mask).unwrap();
        let mut vec_bytes = Vec::with_capacity(512 * 4);
        for &val in &embedding {
            vec_bytes.extend_from_slice(&val.to_ne_bytes());
        }

        conn.execute(
            "INSERT INTO commands_vec (cmd_path, embedding) VALUES (?1, ?2)",
            rusqlite::params![cmd.cmd_path, vec_bytes],
        )
        .unwrap();
    }

    // 2. Run golden cases
    let mut recall_at_1_count = 0;
    let mut reciprocal_ranks_sum = 0.0f64;
    let total_cases = GOLDEN_CASES.len();

    println!("\n=== Starting Golden Dataset Retrieval Evaluation ===");
    for case in GOLDEN_CASES {
        let (ids, mask) = tokenizer.tokenize_query(case.query);
        let query_vec = model.generate_embedding(&ids, &mask).unwrap();

        // Retrieve top 5 matches
        let results = search_commands(&conn, case.query, Some(&query_vec), 5).unwrap();

        let mut matched_pos = None;
        for (idx, contract) in results.iter().enumerate() {
            if contract.cmd_path == case.target_cmd {
                matched_pos = Some(idx);
                break;
            }
        }

        match matched_pos {
            Some(pos) => {
                let rank = pos + 1;
                let rr = 1.0f64 / (rank as f64);
                reciprocal_ranks_sum += rr;
                if pos == 0 {
                    recall_at_1_count += 1;
                }
                println!(
                    "Query: \"{}\" -> Found target '{}' at Rank {} (OK)",
                    case.query, case.target_cmd, rank
                );
            }
            None => {
                println!(
                    "Query: \"{}\" -> Failed to find target '{}' in top 5 results",
                    case.query, case.target_cmd
                );
            }
        }
    }

    let recall_at_1 = (recall_at_1_count as f64) / (total_cases as f64);
    let mrr = reciprocal_ranks_sum / (total_cases as f64);

    println!("\n=== Search Tuning Retrievability Performance metrics ===");
    println!(
        "Recall@1: {:.2}% (Threshold: >= 90.00%)",
        recall_at_1 * 100.0
    );
    println!(
        "MRR (Mean Reciprocal Rank): {:.4} (Threshold: >= 0.9500)",
        mrr
    );

    assert!(
        recall_at_1 >= 0.90,
        "Recall@1 is {:.2}%, which is below the 90.00% threshold limit",
        recall_at_1 * 100.0
    );
    assert!(
        mrr >= 0.95,
        "MRR is {:.4}, which is below the 0.9500 threshold limit",
        mrr
    );
}
