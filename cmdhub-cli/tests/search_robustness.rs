use cmdhub_cli::db::{init_db, search_commands};
use cmdhub_cli::inference::EmbeddingModel;
use cmdhub_cli::tokenizer::Tokenizer;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use tempfile::TempDir;

struct TestCommand {
    name: &'static str,
    cmd_path: &'static str,
    description: &'static str,
}

struct RobustnessCase {
    query: &'static str,
    target_cmd: &'static str,
    category: &'static str,
}

// 50 standard Unix commands with distinct descriptions (matching search_precision.rs)
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

// Robustness cases simulating 100+ real-world user search scenarios
static ROBUSTNESS_CASES: &[RobustnessCase] = &[
    // --- 1. Colloquial Intents (40 cases) ---
    RobustnessCase {
        query: "where is my current terminal location",
        target_cmd: "pwd",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "go to my home folder directory",
        target_cmd: "cd",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "create a new folder path recursively",
        target_cmd: "mkdir",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "clean up the shell screen",
        target_cmd: "clear",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "how long has the server been up",
        target_cmd: "uptime",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "print out a text document in stdout",
        target_cmd: "cat",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "look at the end of syslog file",
        target_cmd: "tail",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "show the first few lines of a csv",
        target_cmd: "head",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "find all lines matching a text pattern",
        target_cmd: "grep",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "calculate space used by this directory",
        target_cmd: "du",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "view system dynamic processes live",
        target_cmd: "top",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "check remaining disk space on my drive",
        target_cmd: "df",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "check my system ram utilization",
        target_cmd: "free",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "tell me who I am logged in as",
        target_cmd: "whoami",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "what is the current system date and time",
        target_cmd: "date",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "list history of my terminal commands",
        target_cmd: "history",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "kill an active program using its pid",
        target_cmd: "kill",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "download a binary from an http endpoint",
        target_cmd: "curl",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "download a web page non-interactively",
        target_cmd: "wget",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "make a symbolic link to another file",
        target_cmd: "ln",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "log in to a remote linux machine",
        target_cmd: "ssh",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "transfer files securely over ssh",
        target_cmd: "scp",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "commit code changes in git",
        target_cmd: "git",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "manage local container lifecycle with docker",
        target_cmd: "docker",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "restart a system service in systemd",
        target_cmd: "systemctl",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "display systemd journal logs",
        target_cmd: "journalctl",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "show kernel release version name",
        target_cmd: "uname",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "compare differences between two versions of a file",
        target_cmd: "diff",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "remove empty folder directory",
        target_cmd: "rmdir",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "copy files to a backup disk",
        target_cmd: "cp",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "rename an image file to another name",
        target_cmd: "mv",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "delete files and all its subfolders recursively",
        target_cmd: "rm",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "change mode permissions of shell script",
        target_cmd: "chmod",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "change file ownership to another user",
        target_cmd: "chown",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "compress folders into a zip file",
        target_cmd: "zip",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "unzip and restore files from zip archive",
        target_cmd: "unzip",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "pack a folder into compressed tarball",
        target_cmd: "tar",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "mount usb flash drive partition",
        target_cmd: "mount",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "unmount an active storage disk",
        target_cmd: "umount",
        category: "Colloquial",
    },
    RobustnessCase {
        query: "view network interfaces and ip addresses",
        target_cmd: "ip",
        category: "Colloquial",
    },
    // --- 2. Typos & Misspellings (30 cases) ---
    RobustnessCase {
        query: "makdir test_folder",
        target_cmd: "mkdir",
        category: "Typos",
    },
    RobustnessCase {
        query: "rmdier empty_dir",
        target_cmd: "rmdir",
        category: "Typos",
    },
    RobustnessCase {
        query: "chomd +x script.sh",
        target_cmd: "chmod",
        category: "Typos",
    },
    RobustnessCase {
        query: "chownn root:root file",
        target_cmd: "chown",
        category: "Typos",
    },
    RobustnessCase {
        query: "gerp -i error log.txt",
        target_cmd: "grep",
        category: "Typos",
    },
    RobustnessCase {
        query: "unzipp project.zip",
        target_cmd: "unzip",
        category: "Typos",
    },
    RobustnessCase {
        query: "docker run hello-world",
        target_cmd: "docker",
        category: "Typos",
    },
    RobustnessCase {
        query: "curll -s http://example.com",
        target_cmd: "curl",
        category: "Typos",
    },
    RobustnessCase {
        query: "pign -c 3 8.8.8.8",
        target_cmd: "ping",
        category: "Typos",
    },
    RobustnessCase {
        query: "clear rerminal screen",
        target_cmd: "clear",
        category: "Typos",
    },
    RobustnessCase {
        query: "psx aux",
        target_cmd: "ps",
        category: "Typos",
    },
    RobustnessCase {
        query: "hstry",
        target_cmd: "history",
        category: "Typos",
    },
    RobustnessCase {
        query: "lss -la",
        target_cmd: "ls",
        category: "Typos",
    },
    RobustnessCase {
        query: "pwdd",
        target_cmd: "pwd",
        category: "Typos",
    },
    RobustnessCase {
        query: "cdd ..",
        target_cmd: "cd",
        category: "Typos",
    },
    RobustnessCase {
        query: "tars -xvf package.tar.gz",
        target_cmd: "tar",
        category: "Typos",
    },
    RobustnessCase {
        query: "systmctl status nginx",
        target_cmd: "systemctl",
        category: "Typos",
    },
    RobustnessCase {
        query: "journlctl -u ssh",
        target_cmd: "journalctl",
        category: "Typos",
    },
    RobustnessCase {
        query: "upime",
        target_cmd: "uptime",
        category: "Typos",
    },
    RobustnessCase {
        query: "whoamii",
        target_cmd: "whoami",
        category: "Typos",
    },
    RobustnessCase {
        query: "copiy source dest",
        target_cmd: "cp",
        category: "Typos",
    },
    RobustnessCase {
        query: "mve file newfile",
        target_cmd: "mv",
        category: "Typos",
    },
    RobustnessCase {
        query: "toutch empty.txt",
        target_cmd: "touch",
        category: "Typos",
    },
    RobustnessCase {
        query: "headd -n 10 file",
        target_cmd: "head",
        category: "Typos",
    },
    RobustnessCase {
        query: "taill -f log",
        target_cmd: "tail",
        category: "Typos",
    },
    RobustnessCase {
        query: "dff -h",
        target_cmd: "df",
        category: "Typos",
    },
    RobustnessCase {
        query: "duu -sh",
        target_cmd: "du",
        category: "Typos",
    },
    RobustnessCase {
        query: "kll -9 1234",
        target_cmd: "kill",
        category: "Typos",
    },
    RobustnessCase {
        query: "pkll firefox",
        target_cmd: "pkill",
        category: "Typos",
    },
    RobustnessCase {
        query: "aliass gs='git status'",
        target_cmd: "alias",
        category: "Typos",
    },
    // --- 3. Pinyin / Translation (20 cases) ---
    RobustnessCase {
        query: "shanchu wenjian",
        target_cmd: "rm",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "chaxun jincheng",
        target_cmd: "ps",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "jiazai cipan",
        target_cmd: "mount",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "kaishi systemd jincheng",
        target_cmd: "systemctl",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "jiancha neicun",
        target_cmd: "free",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "qingchu pingmu",
        target_cmd: "clear",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "liechu mulu",
        target_cmd: "ls",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "wenjian duibi",
        target_cmd: "diff",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "xiazai wenjian",
        target_cmd: "curl",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "chuangjian mulu",
        target_cmd: "mkdir",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "fuzhi wenjian",
        target_cmd: "cp",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "yidong wenjian",
        target_cmd: "mv",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "shanchu kong mulu",
        target_cmd: "rmdir",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "xiugai quanxian",
        target_cmd: "chmod",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "xiugai suoyouzhi",
        target_cmd: "chown",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "sousuo wenjian",
        target_cmd: "find",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "ping wangluo",
        target_cmd: "ping",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "dakai yuancheng shell",
        target_cmd: "ssh",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "yasuo wenjian",
        target_cmd: "zip",
        category: "Pinyin",
    },
    RobustnessCase {
        query: "jieya zip",
        target_cmd: "unzip",
        category: "Pinyin",
    },
    // --- 4. CLI Snippets & Params (20 cases) ---
    RobustnessCase {
        query: "rm -rf /tmp/test",
        target_cmd: "rm",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "mkdir -p /app/src",
        target_cmd: "mkdir",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "chmod 755 -R /var/www",
        target_cmd: "chmod",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "chown -R www-data:www-data",
        target_cmd: "chown",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "tar -xzvf archive.tar.gz",
        target_cmd: "tar",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "unzip -q -d /opt archive.zip",
        target_cmd: "unzip",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "docker run -d --name web -p 80:80 nginx",
        target_cmd: "docker",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "systemctl restart docker.service",
        target_cmd: "systemctl",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "journalctl -xe -u nginx",
        target_cmd: "journalctl",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "grep -rnw '/path' -e 'pattern'",
        target_cmd: "grep",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "ls -lhart",
        target_cmd: "ls",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "ping -i 5 192.168.1.1",
        target_cmd: "ping",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "curl -H 'Content-Type: application/json' -X POST",
        target_cmd: "curl",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "wget --mirror -p --convert-links",
        target_cmd: "wget",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "ssh -i key.pem user@host",
        target_cmd: "ssh",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "scp -r local_dir user@host:/remote_dir",
        target_cmd: "scp",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "git commit -am 'feat: new feature'",
        target_cmd: "git",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "diff -u file1 file2",
        target_cmd: "diff",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "find . -type f -name '*.log'",
        target_cmd: "find",
        category: "CLI Snippets",
    },
    RobustnessCase {
        query: "ln -s /source /target",
        target_cmd: "ln",
        category: "CLI Snippets",
    },
];

// Negative test cases (queries completely unrelated to standard Unix commands)
static NEGATIVE_CASES: &[&str] = &[
    "how to bake a chocolate cake",
    "weather forecast tomorrow in Paris",
    "who is the current president of the USA",
    "banana apple orange watermelon fresh juice",
    "calculate the distance from earth to moon in miles",
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

struct CategoryStats {
    total: usize,
    recall_at_1: usize,
    recall_at_5: usize,
    reciprocal_ranks_sum: f64,
}

#[tokio::test]
async fn test_search_robustness_user_simulation() {
    std::env::set_var("CMDH_OOD_GATE", "1");
    let model_path = find_bge_model_path();
    if !model_path.exists() {
        eprintln!(
            "Skipping search robustness test: ONNX model does not exist at {:?}",
            model_path
        );
        return;
    }

    let tokenizer = Tokenizer::new();
    let model = EmbeddingModel::load(&model_path).unwrap();

    // Enable sqlite-vec extension auto-load BEFORE connection opens
    unsafe {
        type SqliteVecInitFn = unsafe extern "C" fn();
        let init_fn: SqliteVecInitFn = sqlite_vec::sqlite3_vec_init;
        #[allow(clippy::missing_transmute_annotations)]
        let _ = rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(init_fn)));
    }

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("robustness_test.db");
    let conn = Connection::open(&db_path).unwrap();
    init_db(&conn).unwrap();

    let _ = conn.execute(cmdhub_shared::CREATE_COMMANDS_VEC_TABLE, []);

    // 1. Seed commands into SQLite
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
                "root",
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

    // 2. Evaluate robustness cases
    let mut stats_map: HashMap<String, CategoryStats> = HashMap::new();

    // Initialize map
    for cat in &["Colloquial", "Typos", "Pinyin", "CLI Snippets"] {
        stats_map.insert(
            cat.to_string(),
            CategoryStats {
                total: 0,
                recall_at_1: 0,
                recall_at_5: 0,
                reciprocal_ranks_sum: 0.0,
            },
        );
    }

    for case in ROBUSTNESS_CASES {
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

        let cat_stats = stats_map.get_mut(case.category).unwrap();
        cat_stats.total += 1;

        if let Some(pos) = matched_pos {
            let rank = pos + 1;
            let rr = 1.0f64 / (rank as f64);
            cat_stats.reciprocal_ranks_sum += rr;
            cat_stats.recall_at_5 += 1;
            if pos == 0 {
                cat_stats.recall_at_1 += 1;
            }
        }
    }

    // 3. Evaluate negative cases
    let mut negative_matches = Vec::new();
    for query in NEGATIVE_CASES {
        let (ids, mask) = tokenizer.tokenize_query(query);
        let query_vec = model.generate_embedding(&ids, &mask).unwrap();

        let results = search_commands(&conn, query, Some(&query_vec), 3).unwrap();

        assert!(
            results.iter().all(|r| r.confidence == "none"),
            "Query \"{}\" should have confidence \"none\", but got \"{}\"",
            query,
            results
                .first()
                .map(|r| r.confidence.as_str())
                .unwrap_or("unknown")
        );

        let matched: Vec<String> = results.into_iter().map(|r| r.cmd_path).collect();
        negative_matches.push((query, matched));
    }

    // 4. Output structured report
    println!(
        "\n======================================================================================"
    );
    println!("                      CMD-HUB SEARCH ROBUSTNESS REPORT");
    println!(
        "======================================================================================"
    );
    println!(
        "{:<20} | {:<8} | {:<10} | {:<10} | {:<8}",
        "Category", "Cases", "Recall@1", "Recall@5", "MRR"
    );
    println!(
        "--------------------------------------------------------------------------------------"
    );

    let mut overall_total = 0;
    let mut overall_recall_at_1 = 0;
    let mut overall_recall_at_5 = 0;
    let mut overall_reciprocal_ranks_sum = 0.0;

    let order = &["Colloquial", "Typos", "Pinyin", "CLI Snippets"];
    for cat in order {
        if let Some(stats) = stats_map.get(*cat) {
            let recall_1_pct = if stats.total > 0 {
                (stats.recall_at_1 as f64 / stats.total as f64) * 100.0
            } else {
                0.0
            };
            let recall_5_pct = if stats.total > 0 {
                (stats.recall_at_5 as f64 / stats.total as f64) * 100.0
            } else {
                0.0
            };
            let mrr = if stats.total > 0 {
                stats.reciprocal_ranks_sum / stats.total as f64
            } else {
                0.0
            };

            println!(
                "{:<20} | {:<8} | {:<9.1}% | {:<9.1}% | {:<8.4}",
                cat, stats.total, recall_1_pct, recall_5_pct, mrr
            );

            overall_total += stats.total;
            overall_recall_at_1 += stats.recall_at_1;
            overall_recall_at_5 += stats.recall_at_5;
            overall_reciprocal_ranks_sum += stats.reciprocal_ranks_sum;
        }
    }

    let overall_recall_1_pct = if overall_total > 0 {
        (overall_recall_at_1 as f64 / overall_total as f64) * 100.0
    } else {
        0.0
    };
    let overall_recall_5_pct = if overall_total > 0 {
        (overall_recall_at_5 as f64 / overall_total as f64) * 100.0
    } else {
        0.0
    };
    let overall_mrr = if overall_total > 0 {
        overall_reciprocal_ranks_sum / overall_total as f64
    } else {
        0.0
    };

    println!(
        "--------------------------------------------------------------------------------------"
    );
    println!(
        "{:<20} | {:<8} | {:<9.1}% | {:<9.1}% | {:<8.4}",
        "Overall", overall_total, overall_recall_1_pct, overall_recall_5_pct, overall_mrr
    );
    println!(
        "======================================================================================"
    );

    println!("\n--- Out of Domain / Negative Query Traces ---");
    for (query, matched) in &negative_matches {
        println!("Query: \"{}\"", query);
        if matched.is_empty() {
            println!("  -> [No Match] (Desired behavior)");
        } else {
            println!("  -> Matches: {:?}", matched);
        }
    }
    println!(
        "======================================================================================\n"
    );

    // Assert a baseline soft limit of 30% Recall@1 to prevent absolute regression of system matching.
    // As database syncs and NLP algorithms optimize, this baseline should grow.
    assert!(
        overall_recall_1_pct >= 30.0,
        "Search robustness overall Recall@1 is below safety threshold: {:.1}%",
        overall_recall_1_pct
    );
}

#[test]
fn test_search_ood_cli_exit_code_and_stderr() {
    use assert_cmd::Command;
    let mut cmd = Command::cargo_bin("cmdh").unwrap();
    cmd.arg("search").arg("how to bake a chocolate cake");
    cmd.env("CMDH_OOD_GATE", "1");

    let assert = cmd.assert();
    let output = assert.code(2);

    let stderr_str = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    assert!(
        stderr_str
            .contains("No confident match for \"how to bake a chocolate cake\". (out-of-domain)"),
        "stderr was: {}",
        stderr_str
    );

    let stdout_str = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert_eq!(stdout_str.trim(), "[]");
}
