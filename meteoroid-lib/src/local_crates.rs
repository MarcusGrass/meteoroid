use crate::crates::crate_consumer::default::{CrateName, NormalPath, PrunedCrate, RepoName};
use crate::git::CrateReadyForAnalysis;
use crate::{ConsumerOpts, StopReceiver, unpack};
use anyhow::{Context, bail};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

pub fn local_crate_find_task(
    path: PathBuf,
    num_analysis_concurrent: NonZeroUsize,
    consumer_opts: ConsumerOpts,
    mut stop_receiver: StopReceiver,
) -> tokio::sync::mpsc::Receiver<CrateReadyForAnalysis> {
    let (send, recv) = tokio::sync::mpsc::channel(num_analysis_concurrent.get() * 2);
    tokio::task::spawn(async move {
        if let Some(Err(e)) = stop_receiver
            .with_stop(find_local_crates_in(&path, consumer_opts, send))
            .await
        {
            tracing::error!("local crates task error: {}", unpack(&*e));
        } else {
            tracing::debug!("local crates task finished/stopped");
        }
    });
    recv
}

async fn find_local_crates_in(
    path: &Path,
    consumer_opts: ConsumerOpts,
    sender: tokio::sync::mpsc::Sender<CrateReadyForAnalysis>,
) -> anyhow::Result<()> {
    let mut rd = tokio::fs::read_dir(path)
        .await
        .with_context(|| format!("failed to read dir {} searching for crates", path.display()))?;
    let mut max_crates = consumer_opts.max_crates;
    loop {
        let Some(next) = rd.next_entry().await.with_context(|| {
            format!(
                "failed to read next dirent {} searching for crates",
                path.display()
            )
        })?
        else {
            break;
        };
        let ent_path = next.path();
        let metadata = next.metadata().await.with_context(|| {
            format!(
                "failed to read metadata for {} searching for crates",
                ent_path.display()
            )
        })?;
        if !metadata.is_dir() {
            continue;
        }
        match verify_crate_in(ent_path.clone()).await {
            Ok(crate_info) => {
                if let Some(repo) = crate_info.pruned_crate.repository.as_ref() {
                    let mut skip = false;
                    for excl in &consumer_opts.exclude_repository_contains {
                        if repo.0.as_str().contains(excl) {
                            skip = true;
                            break;
                        }
                    }
                    if skip {
                        continue;
                    }
                }
                let mut skip = false;
                for excl in &consumer_opts.exclude_crate_name_contains {
                    let os = crate_info.pruned_crate.crate_name.0.0.as_os_str();
                    // Best effort
                    if let Some(s) = os.to_str()
                        && s.contains(excl)
                    {
                        skip = true;
                        break;
                    }
                }
                if skip {
                    continue;
                }
                if sender.send(crate_info).await.is_err() {
                    bail!(
                        "failed to send crate info for local crate at: {}",
                        ent_path.display()
                    )
                }
                max_crates = max_crates.saturating_sub(1);
                if max_crates == 0 {
                    tracing::debug!("max crates reached, stopping local analysis");
                    return Ok(());
                }
            }
            Err(e) => {
                tracing::warn!("failed to verify crate at {}: {}", ent_path.display(), e);
            }
        }
    }
    Ok(())
}

async fn verify_crate_in(path: PathBuf) -> anyhow::Result<CrateReadyForAnalysis> {
    let ct = path.join("Cargo.toml");
    let content = tokio::fs::read(&ct)
        .await
        .with_context(|| format!("failed to read Cargo.toml at {}", ct.display()))?;
    let _parsed_cargo_toml = cargo_toml::Manifest::from_slice(&content)
        .with_context(|| format!("failed to parse cargo toml at {}", ct.display()))?;
    let p = path
        .components()
        .next_back()
        .with_context(|| format!("failed to get last path component of {}", path.display()))?;
    let crate_name = PathBuf::from(p.as_os_str());
    let crate_name = NormalPath::from_checked_path(crate_name);
    let (git_repo, head_branch) = match crate::git::scan_git_repo(&path).await {
        Ok((repo, head_branch)) => (Some(repo), Some(head_branch)),
        Err(e) => {
            tracing::debug!("failed to scan git repo at {}: {}", path.display(), e);
            (None, None)
        }
    };
    Ok(CrateReadyForAnalysis {
        repo_root: path,
        head_branch,
        pruned_crate: PrunedCrate {
            crate_name: CrateName(crate_name.clone()),
            repository: git_repo,
            repo_dir_name: RepoName(crate_name),
        },
    })
}
