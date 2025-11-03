use crate::StopReceiver;
use crate::cmd::output_string;
use crate::crates::crate_consumer::default::{GitRepo, PrunedCrate};
use crate::error::unpack;
use crate::fs::{Workdir, has_rust_toolchain, has_top_level_cargo_toml};
use anyhow::{Context, bail};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use url::Url;

pub(crate) struct CrateReadyForAnalysis {
    pub(crate) repo_root: PathBuf,
    pub(crate) head_branch: Option<String>,
    pub(crate) pruned_crate: PrunedCrate,
}

pub(crate) fn run_sync_task(
    workdir: Workdir,
    should_sync: bool,
    crates: Vec<PrunedCrate>,
    max_concurrent: NonZeroUsize,
    mut stop_receiver: StopReceiver,
) -> tokio::sync::mpsc::Receiver<CrateReadyForAnalysis> {
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
    sender: tokio::sync::mpsc::Sender<CrateReadyForAnalysis>,
) -> anyhow::Result<()> {
    for cr in crates {
        let Some(repo) = cr.repository.as_ref() else {
            continue;
        };
        let dir = workdir.base.join(cr.repo_dir_name.as_path());
        tracing::trace!(
            "ensuring crate '{}' exists at {} with source {}",
            cr.crate_name,
            dir.display(),
            repo,
        );
        match ensure_at(&dir, repo.as_url()).await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(
                    "failed to ensure crate '{}' at {} with source {}: {}",
                    cr.crate_name,
                    dir.display(),
                    repo,
                    unpack(&*e)
                );
                continue;
            }
        }
        let (head_branch, top_level_cargo_toml, rust_toolchain_toml) = tokio::join!(
            find_remote_head_branch(&dir, "origin"),
            has_top_level_cargo_toml(&dir),
            has_rust_toolchain(&dir)
        );
        let head_branch = match head_branch {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(
                    "failed to find remote head branch for crate '{}' at {} with source {}: {}",
                    cr.crate_name,
                    dir.display(),
                    repo,
                    unpack(&*e)
                );
                continue;
            }
        };
        if !top_level_cargo_toml? {
            tracing::warn!("skipping {}, no Cargo.toml at top-level", cr.crate_name);
            continue;
        }
        if rust_toolchain_toml? {
            tracing::warn!(
                "skipping {}, has rust-toolchain specified (causes issues)",
                cr.crate_name
            );
            continue;
        }
        if should_sync && let Err(e) = sync_existing(&dir, &head_branch).await {
            tracing::error!(
                "failed to sync crate '{}' at {} with source {}: {}",
                cr.crate_name,
                dir.display(),
                repo,
                unpack(&*e)
            );
        }
        if sender
            .send(CrateReadyForAnalysis {
                repo_root: dir,
                head_branch: Some(head_branch),
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

async fn git_remote_show(cwd: &Path, remote: &str) -> anyhow::Result<String> {
    output_string(
        Command::new("git")
            .arg("remote")
            .arg("show")
            .arg(remote)
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(cwd),
    )
    .await
    .with_context(|| format!("failed to run git remote show at '{}'", cwd.display()))
}

async fn find_remote_head_branch(cwd: &Path, remote: &str) -> anyhow::Result<String> {
    let output = git_remote_show(cwd, remote).await?;
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

struct RemoteOutput {
    head_branch: String,
    fetch_url: Url,
}

fn parse_remote_output(output: &str) -> anyhow::Result<RemoteOutput> {
    let mut head_branch = None;
    let mut fetch_url = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("HEAD branch:") {
            let branch = line.split_once(':').unwrap().1.trim();
            head_branch = Some(branch.to_string());
        } else if trimmed.starts_with("Fetch URL:") {
            let repo_url = line.split_once(':').unwrap().1.trim();
            let repo_url = Url::parse(repo_url).with_context(|| {
                format!("failed to parse remote fetch URL from '{repo_url}' at '{line}'")
            })?;
            fetch_url = Some(repo_url);
        }
    }
    Ok(RemoteOutput {
        head_branch: head_branch
            .with_context(|| format!("failed to parse remote HEAD branch from '{output}'"))?,
        fetch_url: fetch_url
            .with_context(|| format!("failed to parse fetch url from '{output}'"))?,
    })
}

pub(crate) async fn scan_git_repo(repo_root: &Path) -> anyhow::Result<(GitRepo, String)> {
    let output = output_string(
        Command::new("git")
            .arg("remote")
            .arg("show")
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(repo_root),
    )
    .await
    .with_context(|| {
        format!(
            "failed to run 'git remote show' at '{}'",
            repo_root.display()
        )
    })?;
    // 128 is 'no git repo' could check for that instead of always returning an error (turn into optional instead)
    let remote = guess_remote_from_show_output(&output).with_context(|| {
        format!(
            "failed to guess remote from 'git remote show' output at '{}'",
            repo_root.display()
        )
    })?;
    let remote_output = git_remote_show(repo_root, &remote).await?;
    let remote_output = parse_remote_output(&remote_output).with_context(|| {
        format!(
            "failed to parse remote output from 'git remote show' output at '{}'",
            repo_root.display()
        )
    })?;
    Ok((GitRepo(remote_output.fetch_url), remote_output.head_branch))
}

fn guess_remote_from_show_output(output: &str) -> Option<String> {
    let mut last_seen_remote = None;
    // Hoping to find `origin`, apart from that, hoping only a single remote exists
    // if neither of those is true, this will be weird, since it's just grabbing the
    // last seen remote. (sorted alphabetically by `git` I think).
    for line in output.lines() {
        if line.trim() == "origin" {
            return Some("origin".to_string());
        }
        last_seen_remote = Some(line.trim().to_string());
    }
    last_seen_remote
}
