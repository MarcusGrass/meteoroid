use crate::error::unpack;
use anyhow::{Context, bail};
use std::fs::Metadata;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub(crate) struct Workdir {
    pub(crate) base: PathBuf,
    pub(crate) versions_csv: PathBuf,
    pub(crate) crates_csv: PathBuf,
}

impl Workdir {
    pub(crate) fn new(base: PathBuf) -> Self {
        Self {
            versions_csv: base.join("versions.csv"),
            crates_csv: base.join("crates.csv"),
            base,
        }
    }

    pub(crate) async fn ensure_workdir(&self) -> anyhow::Result<()> {
        if tokio::fs::try_exists(&self.base).await.with_context(|| {
            format!(
                "failed to check if workdir exists at {}",
                self.base.display()
            )
        })? {
            tracing::debug!("found existing workdir at {}", self.base.display());
        } else {
            tokio::fs::create_dir_all(&self.base)
                .await
                .with_context(|| format!("failed to create workdir at {}", self.base.display()))?;
            tracing::debug!("created workdir at {}", self.base.display());
        }
        Ok(())
    }

    pub(crate) async fn needs_crates_refetch(
        &self,
        staleness_limit_days: u8,
    ) -> anyhow::Result<bool> {
        Ok(needs_refetch(&self.crates_csv, staleness_limit_days).await?
            || needs_refetch(&self.versions_csv, staleness_limit_days).await?)
    }
}

async fn needs_refetch(path: &PathBuf, staleness_limit_days: u8) -> anyhow::Result<bool> {
    match tokio::fs::metadata(&path).await {
        Ok(md) => {
            let Some(lu) = last_updated(&md) else {
                tracing::warn!(
                    "failed to read last updated on {}, considering index stale, will re-fetch",
                    path.display()
                );
                return Ok(true);
            };
            let diff = match SystemTime::now().duration_since(lu) {
                Ok(diff) => diff,
                Err(e) => {
                    tracing::warn!(
                        "failed to compare last update {lu:?} with current time, considering index stale, will re-fetch: {}",
                        unpack(&e)
                    );
                    return Ok(true);
                }
            };
            if diff > Duration::from_secs(3600 * 24 * u64::from(staleness_limit_days)) {
                tracing::info!(
                    "{} is stale (fetched {} seconds ago)",
                    diff.as_secs(),
                    path.display()
                );
                return Ok(true);
            }
            tracing::debug!(
                "{} does not need a refetch (fetched {} seconds ago)",
                path.display(),
                diff.as_secs()
            );
            Ok(false)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(true),
        Err(e) => {
            bail!("failed to read {}: {}", path.display(), unpack(&e))
        }
    }
}

fn last_updated(metadata: &Metadata) -> Option<std::time::SystemTime> {
    metadata
        .modified()
        .ok()
        .or_else(|| metadata.accessed().ok())
        .or_else(|| metadata.created().ok())
}

pub(crate) async fn has_top_level_cargo_toml(repo_root: &Path) -> anyhow::Result<bool> {
    let path = repo_root.join("Cargo.toml");
    tokio::fs::try_exists(&path)
        .await
        .with_context(|| format!("failed to check for Cargo.toml at {}", path.display()))
}

pub(crate) async fn has_rust_toolchain(repo_root: &Path) -> anyhow::Result<bool> {
    let rust_toolchain_classic = repo_root.join("rust-toolchain");
    if tokio::fs::try_exists(&rust_toolchain_classic)
        .await
        .with_context(|| {
            format!(
                "failed to check for rust-toolchain at {}",
                rust_toolchain_classic.display()
            )
        })?
    {
        return Ok(true);
    }
    let rust_toolchain_toml = repo_root.join("rust-toolchain.toml");
    tokio::fs::try_exists(&rust_toolchain_toml)
        .await
        .with_context(|| {
            format!(
                "failed to check for rust-toolchain at {}",
                rust_toolchain_toml.display()
            )
        })
}
