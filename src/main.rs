#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable
)]

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use digikam2lightcraft::{
    adapter::{self, Faces, MetadataSource, Options},
    bundle, inventory, io, migrate,
    model::RootMapping,
    source::Source,
};

#[derive(Parser)]
#[command(
    version,
    about = "Preserve, inspect and replay digiKam metadata into LightCraft"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Inventory active records and missing paths. Never modifies the source.
    Inspect {
        #[command(flatten)]
        source: SourceArgs,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Make a durable target-independent snapshot of database metadata and XMP sidecars.
    Extract {
        #[command(flatten)]
        source: SourceArgs,
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        sample: Option<usize>,
        #[arg(long)]
        no_sidecars: bool,
    },
    /// Create or safely refresh a managed LightCraft library, referencing originals in place.
    Import {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        library: PathBuf,
        #[arg(long, value_enum, default_value = "named")]
        faces: Faces,
        #[arg(long, value_enum, default_value = "database")]
        metadata_source: MetadataSource,
        #[arg(long,value_parser=parse_root)]
        root: Vec<RootMapping>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Find missing originals and uncatalogued media, excluding trash and resource forks.
    Inventory {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Reconcile migration identities and detect subsequent LightCraft changes.
    Verify {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        library: PathBuf,
    },
    /// Describe native mappings, approximations, archived data and upgrade policy.
    Capabilities,
}
#[derive(Args)]
struct SourceArgs {
    /// digiKam core SQLite database (digikam4.db), from a stable backup without an active WAL.
    #[arg(long)]
    database: PathBuf,
    /// Explicit source root id to local directory mapping, repeatable: --root '1=/Photos'.
    #[arg(long,required=true,value_parser=parse_root)]
    root: Vec<RootMapping>,
}
fn parse_root(raw: &str) -> std::result::Result<RootMapping, String> {
    let (id, path) = raw
        .split_once('=')
        .ok_or("use --root ID=/absolute/photo/root")?;
    let id = id
        .parse::<i64>()
        .map_err(|_| "root id must be an integer")?;
    if path.is_empty() {
        return Err("root path is empty".to_string());
    }
    Ok(RootMapping {
        id,
        path: PathBuf::from(path),
    })
}
fn run() -> Result<()> {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Inspect { source, output } => {
            let source = Source::open(&source.database, source.root)?;
            let report = bundle::inspect(&source)?;
            if let Some(path) = output {
                let mut protected = source
                    .roots
                    .iter()
                    .map(|r| r.path.clone())
                    .collect::<Vec<_>>();
                protected.push(
                    source
                        .path
                        .parent()
                        .context("source database parent")?
                        .to_path_buf(),
                );
                io::new_report(&path, &report, &protected)?;
            }
            report
        }
        Command::Extract {
            source,
            bundle,
            sample,
            no_sidecars,
        } => {
            let source = Source::open(&source.database, source.root)?;
            serde_json::to_value(bundle::extract(&source, &bundle, sample, !no_sidecars)?)?
        }
        Command::Import {
            bundle,
            library,
            faces,
            metadata_source,
            root,
            limit,
        } => {
            ensure!(limit != Some(0), "limit must be positive");
            serde_json::to_value(migrate::import(
                &bundle,
                &library,
                Options {
                    faces,
                    metadata_source,
                    roots: root,
                },
                limit,
            )?)?
        }
        Command::Inventory { bundle, output } => {
            let report = inventory::inspect(&bundle)?;
            if let Some(path) = output {
                let manifest: digikam2lightcraft::model::Manifest = serde_json::from_slice(
                    &io::read_bounded(&bundle.join("manifest.json"), 4 * 1024 * 1024)?,
                )?;
                let mut protected = manifest
                    .roots
                    .into_iter()
                    .map(|r| r.path)
                    .collect::<Vec<_>>();
                protected.push(
                    manifest
                        .source_db
                        .parent()
                        .context("source database parent")?
                        .to_path_buf(),
                );
                protected.push(bundle);
                io::new_report(&path, &report, &protected)?;
            }
            report
        }
        Command::Verify { bundle, library } => migrate::verify(&bundle, &library)?,
        Command::Capabilities => adapter::capabilities(),
    };
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer_pretty(&mut writer, &result).context("write command result")?;
    use std::io::Write;
    writeln!(writer)?;
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("digikam2lightcraft: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
