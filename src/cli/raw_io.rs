//! `llmux raw-io` — operate the day-partitioned raw-io store
//! (`proxy::raw_store`): status, import (backups / legacy files / archives),
//! restore an archived day, seal past days now. Works with or without a
//! running daemon (SQLite + file locks coordinate the two); acts on the LOCAL
//! store only, so it is refused in remote mode.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::CliError;
use crate::proxy::raw_store::{ImportOptions, RawStore, DEFAULT_MIN_FREE_BYTES};

#[derive(Debug, Args)]
pub struct RawIoArgs {
    #[command(subcommand)]
    pub command: RawIoCommand,
}

#[derive(Debug, Subcommand)]
pub enum RawIoCommand {
    /// Show hot partitions, archives, spool/legacy files and import progress.
    Status(StoreArg),
    /// Import jsonl / jsonl.zst files — read as ONE concatenated stream, so a
    /// backup split into byte ranges imports with the original's offsets.
    /// Re-running is safe: rows are deduplicated by source occurrence.
    Import(ImportCmd),
    /// Bring an archived day back into a hot partition (the archive stays).
    Restore(RestoreCmd),
    /// Seal days older than `--hot-days` into verified archives now.
    Archive(ArchiveCmd),
}

#[derive(Debug, Args)]
pub struct StoreArg {
    /// Store root (default `$XDG_STATE_HOME/llmux/store/raw-io`).
    #[arg(long, value_name = "DIR")]
    pub store: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ImportCmd {
    /// Files to import, in stream order (plain or zstd).
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// Only import records of this local day or later (YYYY-MM-DD).
    #[arg(long, value_name = "DAY")]
    pub from: Option<String>,
    /// Only import records of this local day or earlier (YYYY-MM-DD).
    #[arg(long, value_name = "DAY")]
    pub to: Option<String>,
    /// Pause when free disk drops below this many GiB (resumable).
    #[arg(long, default_value_t = DEFAULT_MIN_FREE_BYTES >> 30)]
    pub min_free_gb: u64,
    #[command(flatten)]
    pub store: StoreArg,
}

#[derive(Debug, Args)]
pub struct RestoreCmd {
    /// The archived day (YYYY-MM-DD).
    pub day: String,
    #[arg(long, default_value_t = DEFAULT_MIN_FREE_BYTES >> 30)]
    pub min_free_gb: u64,
    #[command(flatten)]
    pub store: StoreArg,
}

#[derive(Debug, Args)]
pub struct ArchiveCmd {
    /// Past days to keep hot (min 1: today and yesterday may still be open
    /// in a running daemon). Default: `raw_io.hot_days` from the config.
    #[arg(long)]
    pub hot_days: Option<u64>,
    #[command(flatten)]
    pub store: StoreArg,
}

fn open_store(arg: &StoreArg) -> Result<std::sync::Arc<RawStore>, CliError> {
    match &arg.store {
        Some(root) => Ok(std::sync::Arc::new(RawStore::with_root(
            root.clone(),
            root.join(".no-legacy.jsonl"),
        ))),
        None => {
            let path = super::daemon::raw_io_path().ok_or_else(|| {
                CliError::Message("could not determine the llmux state directory".into())
            })?;
            Ok(crate::proxy::raw_store::store_for(&path))
        }
    }
}

fn valid_day(day: &str) -> Result<(), CliError> {
    let ok = day.len() == 10
        && day.bytes().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
    if ok {
        Ok(())
    } else {
        Err(CliError::Message(format!("not a YYYY-MM-DD day: {day:?}")))
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| CliError::Message(format!("serialize: {e}")))?;
    println!("{text}");
    Ok(())
}

fn store_err(e: crate::proxy::raw_store::StoreError) -> CliError {
    CliError::Message(format!("raw-io: {e}"))
}

pub async fn run(args: RawIoArgs) -> Result<(), CliError> {
    tokio::task::spawn_blocking(move || run_blocking(args))
        .await
        .map_err(|e| CliError::Message(format!("raw-io task failed: {e}")))?
}

fn run_blocking(args: RawIoArgs) -> Result<(), CliError> {
    match args.command {
        RawIoCommand::Status(arg) => print_json(&open_store(&arg)?.status()),
        RawIoCommand::Import(cmd) => {
            for day in cmd.from.iter().chain(cmd.to.iter()) {
                valid_day(day)?;
            }
            let store = open_store(&cmd.store)?;
            let outcome = store
                .import_files(
                    &cmd.files,
                    &ImportOptions {
                        from_day: cmd.from,
                        to_day: cmd.to,
                        min_free_bytes: cmd.min_free_gb << 30,
                    },
                )
                .map_err(store_err)?;
            print_json(&outcome)
        }
        RawIoCommand::Restore(cmd) => {
            valid_day(&cmd.day)?;
            let store = open_store(&cmd.store)?;
            let outcomes = store
                .restore_day(&cmd.day, cmd.min_free_gb << 30)
                .map_err(store_err)?;
            print_json(&outcomes)
        }
        RawIoCommand::Archive(cmd) => {
            let hot_days = match cmd.hot_days {
                Some(n) => n,
                None => crate::config::load_or_init()?.raw_io.hot_days,
            }
            .max(1);
            let store = open_store(&cmd.store)?;
            let done = store
                .archive_cycle(crate::proxy::raw_io::now_ms(), hot_days, false)
                .map_err(store_err)?;
            print_json(&done)
        }
    }
}
