use anyhow::Result;
use clap::{Parser, Subcommand};

pub mod config;
pub mod db;
pub mod dto;
pub mod inference;
pub mod installer;
pub mod os_detector;
pub mod runner;
pub mod tokenizer;
pub mod updater;

#[derive(Parser, Debug)]
#[command(
    name = "cmdh",
    about = "cmdh — the CmdHub CLI client for offline command search and execution",
    version
)]
pub struct Cli {
    /// Custom configuration file path
    #[arg(short, long, global = true, help = "Custom configuration file path")]
    pub config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Search offline ACI commands via hybrid FTS5 & Vector search
    Search {
        /// The query string to search for
        query: String,
        /// Maximum number of search results to return
        #[arg(long, default_value_t = 1)]
        limit: usize,
        /// Force full preset output format
        #[arg(short, long, group = "output_format")]
        full: bool,
        /// Force usage preset output format
        #[arg(short, long, group = "output_format")]
        usage_only: bool,
        /// Force minimal preset output format
        #[arg(short, long, group = "output_format")]
        minimal: bool,
    },
    /// Sync the offline SQLite database from CDN
    Update {
        /// Force database download and sync
        #[arg(long)]
        force: bool,
    },
    /// Safety-wrapped execution sandbox for Agents
    Run {
        /// Materizalized command path to execute (e.g. "tar.extract")
        cmd_path: String,
        /// Arguments passed directly to the underlying CLI tool
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Bypass human interactive gating check for dangerous commands
        #[arg(short, long)]
        yes: bool,
    },
    /// Install assets or models
    Install {
        #[command(subcommand)]
        sub: InstallAction,
    },
    /// Initialize a new config.toml file with default systems properties
    Init {
        /// Overwrite config file if it already exists
        #[arg(long)]
        force: bool,
    },
    /// Generate shell autocompletion script to stdout
    Completions {
        /// Shell type (bash, zsh, fish)
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum InstallAction {
    /// Install BGE vector model for local semantic search
    Vector {
        /// Install from local file instead of downloading
        #[arg(long, value_name = "FILE")]
        from_file: Option<std::path::PathBuf>,
        /// Force install/reinstall even if SHA-256 matches
        #[arg(long)]
        force: bool,
    },
}

pub async fn run() -> Result<()> {
    // 1. Parse command line arguments
    let cli = Cli::parse();

    // Run setup initialization before loading configuration or opening DB
    if let Commands::Init { force } = cli.command {
        let config_dir = config::get_config_dir();
        let config_path = config_dir.join("config.toml");

        if config_path.exists() && !force {
            eprintln!(
                "Warning: Configuration file already exists at {:?}",
                config_path
            );
            return Ok(());
        }

        std::fs::create_dir_all(&config_dir)?;

        let detected = os_detector::detect_os().unwrap_or_else(|| "unknown".to_string());
        let default_key: String = config::OFFICIAL_PUBLIC_KEY
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        let config_content = format!(
            r#"# CmdHub configuration file
api_url = "https://api.cmdhub.xyz"
public_key = "{default_key}"
timeout_seconds = 30

[output]
# Set the format of the search results output to stdout.
# Supported modes:
#  - "full"   : Returns the full command contract including descriptions, risks, and install commands.
#  - "usage"  : Returns a slim template format focusing purely on path and execution usage structure.
#  - "minimal": Returns only the command pathway (e.g. [{{"cmd_path":"git"}}]).
mode = "full"

[install]
# Host operating system override.
# Detected on your platform as: "{detected}"
# To override manually, uncomment the line below:
# os = "{detected}"

# Priority sequence when searching for package manager installer instructions.
# The resolver checks system installers first (matching your OS release),
# then traverses these developer packages in order.
package_managers = ["uv", "npm", "cargo", "go"]
"#
        );

        std::fs::write(&config_path, config_content)?;
        println!(
            "Configuration initialized successfully at {:?}",
            config_path
        );
        return Ok(());
    }

    // 2. Load config with CLI override path
    let config = config::load_or_create_config(cli.config.clone())?;

    // 3. Open DB connection and ensure initialized
    let conn = db::open_db()?;
    db::init_db(&conn)?;

    match cli.command {
        Commands::Search {
            query,
            limit,
            full,
            usage_only,
            minimal,
        } => {
            let default_path = config::get_data_dir().join("models/bge-micro-v2.onnx");
            let model_path = config
                .vector
                .model_path
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or(default_path);

            let mut query_vector = None;
            if model_path.exists() {
                if let Ok(model) = inference::EmbeddingModel::load(&model_path) {
                    let tokenizer = tokenizer::Tokenizer::new();
                    let (ids, mask) = tokenizer.tokenize_query(&query);
                    if let Ok(vec) = model.generate_embedding(&ids, &mask) {
                        query_vector = Some(vec);
                    }
                }
            } else {
                eprintln!(
                    "Tip: Semantic search is inactive. Run 'cmdh install vector' to activate."
                );
            }

            let results = db::search_all(&conn, &query, query_vector.as_deref(), limit)?;

            let mode = if full {
                "full"
            } else if usage_only {
                "usage"
            } else if minimal {
                "minimal"
            } else {
                &config.output.mode
            };

            let json_output = dto::format_results(results, mode, &config);
            // Output pure JSON data strictly to STDOUT so AI agents can pipe to jq
            println!("{}", serde_json::to_string(&json_output)?);
        }
        Commands::Update { force } => {
            updater::update_database(&config, force).await?;
        }
        Commands::Run {
            cmd_path,
            args,
            yes,
        } => {
            runner::run_command(&conn, &cmd_path, &args, yes)?;
        }
        Commands::Install { sub } => match sub {
            InstallAction::Vector { from_file, force } => {
                installer::install_vector(&config, from_file, force).await?;
            }
        },
        Commands::Completions { shell } => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "cmdh", &mut std::io::stdout());
        }
        Commands::Init { .. } => unreachable!(),
    }

    Ok(())
}
