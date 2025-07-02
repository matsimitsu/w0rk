use base::{Config, Workspace};
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use sync::Syncer;

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    New,
    Sync,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let proj_dirs = match ProjectDirs::from("com", "matsimitsu", "w0rk") {
        Some(proj_dirs) => proj_dirs,
        None => {
            return Err(anyhow::anyhow!("Could not find project directories"));
        }
    };
    let config_path = proj_dirs.config_dir().join("config.json");
    println!("Config path: {:?}", config_path);
    let config = Config::from_path(&config_path)?;
    let workspace = Workspace::from_path(&config.work_dir)?;

    match &cli.command {
        Commands::New => {
            let new_day = workspace.new_day()?;
            println!("New day: {:?}", new_day.path);
        }
        Commands::Sync => {
            let syncer = Syncer::new(&config, proj_dirs.data_local_dir(), &workspace)?;
            syncer.sync().await?;

            println!("Syncing...");
        }
    }

    Ok(())
}
