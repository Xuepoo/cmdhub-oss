use anyhow::Result;
use clap::{Parser, Subcommand};

pub mod config;
pub mod db;
pub mod inference;
pub mod runner;
pub mod installer;
pub mod os_detector;
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

    // 2. Load config with CLI override path
    let config = config::load_or_create_config(cli.config.clone())?;

    // 3. Open DB connection and ensure initialized
    let conn = db::open_db()?;
    db::init_db(&conn)?;

    match cli.command {
        Commands::Search { query, limit } => {
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
            // Output pure JSON data strictly to STDOUT so AI agents can pipe to jq
            println!("{}", serde_json::to_string(&results)?);
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
    }

    Ok(())
}
