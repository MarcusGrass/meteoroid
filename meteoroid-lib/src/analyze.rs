pub(crate) mod report;

use crate::analyze::report::{CrateAnalysis, RustfmtAnalysis};
use crate::cmd::{RustFmtBuildOutputs, RustfmtOutput, run_rustfmt};
use crate::git::GitSyncedCrate;
use anyhow::Context;
use dashmap::DashSet;
use rustc_hash::FxBuildHasher;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct AnalyzeArgs {
    pub rustfmt_repo: PathBuf,
    pub rustfmt_upstream_repo: PathBuf,
    pub report_dest: Option<PathBuf>,
    pub config: Option<String>,
    pub write_outputs: bool,
    pub include_non_diverging_crates: bool,
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn analyze_crate(
    target: &GitSyncedCrate,
    rustfmt_build_outputs: &RustFmtBuildOutputs,
    upstream_rustfmt_build_outputs: &RustFmtBuildOutputs,
    config: Option<&str>,
    seen: Arc<DashSet<String, FxBuildHasher>>,
    timeout: Duration,
) -> anyhow::Result<Vec<CrateAnalysis>> {
    tracing::trace!("analyzing '{}'", target.pruned_crate.crate_name);
    let mut analyses = vec![];
    let Some(members) = crate::cargo::read_members(&target.repo_root).await? else {
        tracing::trace!(
            "found no Cargo.toml for '{}'",
            target.pruned_crate.crate_name
        );
        return Ok(analyses);
    };
    for workspace in members.roots {
        let ident = format!("{}:{}", target.pruned_crate.repository, workspace.display());
        if !seen.insert(ident) {
            tracing::trace!(
                "skipping seen workspace {} at {}",
                workspace.display(),
                target.pruned_crate.repository
            );
            continue;
        }
        let src = workspace.join("src");
        if !tokio::fs::try_exists(&src)
            .await
            .with_context(|| format!("failed to check if {} exists", src.display()))?
        {
            tracing::trace!(
                "skipping {}, has no `src` subdirectory",
                workspace.display()
            );
            continue;
        }
        let mut ca = CrateAnalysis::new(
            target.pruned_crate.crate_name.clone(),
            workspace.clone(),
            target.pruned_crate.repository.clone(),
            target.head_branch.clone(),
        );
        let TimedOutput { output, elapsed } = timed(run_local_rustfmt_build(
            &target.repo_root,
            upstream_rustfmt_build_outputs,
            config,
            timeout,
        ))
        .await;
        let (upstream_diff_output, rustfmt_error) = match output {
            Ok(None) => {
                tracing::trace!("upstream rustfmt succeeded");
                (None, None)
            }
            Ok(Some(diff)) => {
                tracing::debug!("upstream rustfmt has diff");
                (Some(diff), None)
            }
            Err(e) => {
                tracing::warn!("upstream rustfmt failed on {}", src.display());
                (None, Some(e))
            }
        };
        ca.upstream_rustfmt_analysis = Some(RustfmtAnalysis {
            diff_output: upstream_diff_output.clone(),
            rustfmt_error,
            elapsed,
        });
        let TimedOutput { output, elapsed } = timed(run_local_rustfmt_build(
            &target.repo_root,
            rustfmt_build_outputs,
            config,
            timeout,
        ))
        .await;
        let (local_diff_output, rustfmt_error) = match output {
            Ok(None) => {
                if upstream_diff_output.is_some() {
                    ca.diverving_diff = true;
                    tracing::info!(
                        "local rustfmt didn't diff while upstream rustfmt did on '{}'({})",
                        target.pruned_crate.crate_name,
                        src.display()
                    );
                }
                (None, None)
            }
            Ok(Some(d)) => {
                if let Some(upstream_diff_output) = upstream_diff_output {
                    if upstream_diff_output == d {
                        tracing::debug!(
                            "local rustfmt has same diff as upstream on '{}'",
                            src.display()
                        );
                    } else {
                        tracing::info!(
                            "local rustfmt and upstream rustfmt diffed on '{}'({}), but the diffs where not the same",
                            target.pruned_crate.crate_name,
                            src.display()
                        );
                        ca.diverving_diff = true;
                    }
                } else {
                    ca.diverving_diff = true;
                    tracing::info!(
                        "local rustfmt diffed on '{}'({}) while upstream didn't",
                        target.pruned_crate.crate_name,
                        src.display()
                    );
                }
                (Some(d), None)
            }
            Err(e) => {
                tracing::warn!("local rustfmt failed on {}", src.display());
                (None, Some(e))
            }
        };
        ca.local_rustfmt_analysis = Some(RustfmtAnalysis {
            diff_output: local_diff_output,
            rustfmt_error,
            elapsed,
        });
        analyses.push(ca);
        match workspace.strip_prefix(&target.repo_root) {
            Ok(ext) => {
                tracing::debug!(
                    "finished {}/{} at {}",
                    target.pruned_crate.crate_name,
                    ext.display(),
                    workspace.display(),
                );
            }
            Err(_e) => {
                tracing::debug!(
                    "finished {} (failed to strip prefix)",
                    target.pruned_crate.crate_name
                );
            }
        }
    }

    Ok(analyses)
}

async fn run_local_rustfmt_build(
    target_repo: &Path,
    rust_fmt_build_outputs: &RustFmtBuildOutputs,
    config: Option<&str>,
    timeout: Duration,
) -> anyhow::Result<Option<String>> {
    let mut cmd = tokio::process::Command::new("cargo");
    cmd.env(
        "LD_LIBRARY_PATH",
        rust_fmt_build_outputs.toolchain_lib_path.ld_library_path(),
    )
    .env("RUSTFMT", &rust_fmt_build_outputs.built_binary_path)
    .env_remove("RUSTUP_TOOLCHAIN")
    .current_dir(target_repo)
    .arg("fmt")
    .arg("--all")
    .arg("--check");
    // For some reason that I can't figure out RUSTUP_TOOLCHAIN gets set and overrides `rustfmt`'s
    // required default
    if let Some(cfg) = config {
        cmd.arg("--").arg("--config").arg(cfg);
    }

    match run_rustfmt(&mut cmd, timeout).await {
        RustfmtOutput::Success => Ok(None),
        RustfmtOutput::Diff(d) => Ok(Some(d)),
        RustfmtOutput::Failure(e) => Err(e),
    }
}

struct TimedOutput<T> {
    output: T,
    elapsed: Duration,
}

async fn timed<F: Future<Output = T>, T>(fut: F) -> TimedOutput<T> {
    let start = Instant::now();
    let output = fut.await;
    TimedOutput {
        output,
        elapsed: start.elapsed(),
    }
}
