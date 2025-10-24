use crate::StopReceiver;
use crate::cmd::output_string;
use crate::crates::crate_consumer::default::PrunedCrate;
use crate::error::unpack;
use crate::fs::Workdir;
use anyhow::{Context, bail};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use url::Url;

pub(crate) struct GitSyncedCrate {
    pub(crate) repo_root: PathBuf,
    pub(crate) head_branch: String,
    pub(crate) pruned_crate: PrunedCrate,
}

pub(crate) fn run_sync_task(
    workdir: Workdir,
    should_sync: bool,
    crates: Vec<PrunedCrate>,
    max_concurrent: NonZeroUsize,
    mut stop_receiver: StopReceiver,
) -> tokio::sync::mpsc::Receiver<GitSyncedCrate> {
    let (send, recv) = tokio::sync::mpsc::channel(max_concurrent.get());
    tokio::task::spawn(async move {
        match stop_receiver
            .with_stop(sync_task(workdir, should_sync, crates, send))
            .await
        {
            None => {
                tracing::info!("sync task was stopped before finishing, exiting");
            }
            Some(Ok(())) => {
                tracing::debug!("sync task finished successfully");
            }
            Some(Err(e)) => {
                tracing::error!("sync task failed: {}", unpack(&*e));
            }
        }
    });
    recv
}

async fn sync_task(
    workdir: Workdir,
    should_sync: bool,
    crates: Vec<PrunedCrate>,
    sender: tokio::sync::mpsc::Sender<GitSyncedCrate>,
) -> anyhow::Result<()> {
    for cr in crates {
        let dir = workdir.base.join(cr.repo_dir_name.as_path());
        tracing::trace!(
            "ensuring crate '{}' exists at {} with source {}",
            cr.crate_name,
            dir.display(),
            cr.repository
        );
        match ensure_at(&dir, cr.repository.as_url()).await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(
                    "failed to ensure crate '{}' at {} with source {}: {}",
                    cr.crate_name,
                    dir.display(),
                    cr.repository,
                    unpack(&*e)
                );
                continue;
            }
        }
        let head_branch = match find_remote_head_branch(&dir).await {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(
                    "failed to find remote head branch for crate '{}' at {} with source {}: {}",
                    cr.crate_name,
                    dir.display(),
                    cr.repository,
                    unpack(&*e)
                );
                continue;
            }
        };
        if should_sync && let Err(e) = sync_existing(&dir, &head_branch).await {
            tracing::error!(
                "failed to sync crate '{}' at {} with source {}: {}",
                cr.crate_name,
                dir.display(),
                cr.repository,
                unpack(&*e)
            );
        }
        if sender
            .send(GitSyncedCrate {
                repo_root: dir,
                head_branch,
                pruned_crate: cr,
            })
            .await
            .is_err()
        {
            bail!("failed to send git synced crate")
        }
    }
    Ok(())
}

pub(crate) async fn ensure_at(path: &Path, repo_url: &Url) -> anyhow::Result<()> {
    if tokio::fs::try_exists(path)
        .await
        .with_context(|| format!("failed to check if '{}' exists", path.display()))?
    {
        tracing::trace!(
            "found existing directory at {}, assuming previously created git repo, skipping clone",
            path.display()
        );
    } else {
        tracing::debug!(
            "no existing crate at {}, cloning from {}",
            path.display(),
            repo_url
        );
        output_string(
            Command::new("git")
                .arg("clone")
                .arg("--depth")
                .arg("1")
                .arg(repo_url.as_str())
                .arg(path)
                .env("GIT_TERMINAL_PROMPT", "0"),
        )
        .await
        .with_context(|| {
            format!(
                "failed to clone repo at '{repo_url}' to '{}'",
                path.display()
            )
        })?;
    }
    Ok(())
}

async fn sync_existing(repo_root: &Path, head_branch: &str) -> anyhow::Result<()> {
    let git_dir = repo_root.join(".git");
    if !tokio::fs::try_exists(&git_dir).await.with_context(|| {
        format!(
            "failed to check if git dir exists at '{}'",
            git_dir.display()
        )
    })? {
        anyhow::bail!(
            "was pointed to a non-git directory at {}",
            repo_root.display()
        )
    }
    tracing::trace!(
        "found existing git repo at {}, syncing",
        repo_root.display()
    );
    output_string(
        Command::new("git")
            .arg("fetch")
            .arg("origin")
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(repo_root),
    )
    .await
    .with_context(|| {
        format!(
            "failed to fetch origin at repo root: {}",
            repo_root.display()
        )
    })?;
    output_string(
        Command::new("git")
            .arg("reset")
            .arg("--hard")
            .arg(format!("origin/{head_branch}"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(repo_root),
    )
    .await?;
    tracing::trace!("synced {} to origin/{head_branch}", repo_root.display());
    Ok(())
}

async fn find_remote_head_branch(cwd: &Path) -> anyhow::Result<String> {
    let output = output_string(
        Command::new("git")
            .arg("remote")
            .arg("show")
            .arg("origin")
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(cwd),
    )
    .await?;
    parse_head_branch(&output)
}

fn parse_head_branch(output: &str) -> anyhow::Result<String> {
    for line in output.lines() {
        if line.contains("HEAD branch:") {
            let branch = line.split_once(':').unwrap().1.trim();
            return Ok(branch.to_string());
        }
    }
    anyhow::bail!(
        "failed to parse remote HEAD branch from 'git remote show origin' output from '{output}'"
    )
}
