mod html;

use crate::analyze::similarity::similarity;
use crate::cmd::{DiffResult, try_diff};
use crate::crates::crate_consumer::default::{CrateName, GitRepo, NormalPath};
use crate::unpack;
use anyhow::Context;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[derive(serde::Serialize)]
pub(crate) struct AnalysisReport {
    #[serde(skip)]
    output: OutputDirs,
    num_diverging_diffs: usize,
    num_upstream_failures: usize,
    num_upstream_diffs: usize,
    num_upstream_successes: usize,
    num_local_failures: usize,
    num_local_diffs: usize,
    num_local_successes: usize,
    crate_reports: Vec<CrateReport>,
}

struct OutputDirs {
    base: PathBuf,
    diverged: PathBuf,
    nondiverged: PathBuf,
    errors: PathBuf,
}

impl Ord for CrateReport {
    fn cmp(&self, other: &Self) -> Ordering {
        // Diverged is top priority
        if self.diverged && !other.diverged {
            return Ordering::Greater;
        } else if !self.diverged && other.diverged {
            return Ordering::Less;
        }
        if self.has_error() && !other.has_error() {
            return Ordering::Greater;
        } else if !self.has_error() && other.has_error() {
            return Ordering::Less;
        }
        if self.has_diff() && !other.has_diff() {
            return Ordering::Greater;
        } else if !self.has_diff() && other.has_diff() {
            return Ordering::Less;
        }
        self.crate_name.cmp(&other.crate_name)
    }
}

impl PartialOrd for CrateReport {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl AnalysisReport {
    pub(crate) async fn new(output_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let output = if let Some(output_dir) = output_dir {
            output_dir
        } else {
            tempfile::tempdir()
                .context("failed to create tempdir")?
                .keep()
        };
        let diverged = output.join("diverged");
        let nondiverged = output.join("nondiverged");
        let errors = output.join("errors");
        let (r1, r2, r3) = tokio::join!(
            tokio::fs::create_dir_all(&diverged),
            tokio::fs::create_dir_all(&nondiverged),
            tokio::fs::create_dir_all(&errors)
        );
        r1.with_context(|| format!("failed to create diverged dir at {}", diverged.display()))?;
        r2.with_context(|| {
            format!(
                "failed to create nondiverged dir at {}",
                nondiverged.display()
            )
        })?;
        r3.with_context(|| format!("failed to create errors dir at {}", errors.display()))?;
        tracing::info!("using output dir at {}", output.display());
        Ok(Self {
            output: OutputDirs {
                base: output,
                diverged,
                nondiverged,
                errors,
            },
            num_diverging_diffs: 0,
            num_upstream_failures: 0,
            num_upstream_diffs: 0,
            num_upstream_successes: 0,
            num_local_failures: 0,
            num_local_diffs: 0,
            num_local_successes: 0,
            crate_reports: vec![],
        })
    }

    pub(crate) async fn add_result(
        &mut self,
        diff_tool: Option<&Path>,
        cr: CrateAnalysis,
        write_outputs: bool,
        skip_non_diverging_diffs: bool,
    ) {
        let pre_errors = self.num_local_failures + self.num_upstream_failures;
        if cr.diverging_diff.diverged() {
            self.num_diverging_diffs += 1;
        }
        let similar_errors = if let (Some(local_err), Some(upstream_err)) = (
            cr.local_rustfmt_analysis.rustfmt_error.as_deref(),
            cr.upstream_rustfmt_analysis.rustfmt_error.as_deref(),
        ) {
            let lerr = local_err.to_string();
            let uerr = upstream_err.to_string();
            similarity(&lerr, &uerr)
        } else {
            false
        };
        let upstream_out = create_rustfmt_output(
            &cr.crate_name,
            &self.output,
            "upstream",
            write_outputs,
            cr.diverging_diff.diverged(),
            cr.upstream_rustfmt_analysis,
            &mut self.num_upstream_successes,
            &mut self.num_upstream_diffs,
            &mut self.num_upstream_failures,
        )
        .await;
        let local_out = create_rustfmt_output(
            &cr.crate_name,
            &self.output,
            "local",
            write_outputs,
            cr.diverging_diff.diverged(),
            cr.local_rustfmt_analysis,
            &mut self.num_local_successes,
            &mut self.num_local_diffs,
            &mut self.num_local_failures,
        )
        .await;
        let meta_diff_file = match cr.diverging_diff {
            DivergingDiff::LocalOnly | DivergingDiff::UpstreamOnly | DivergingDiff::None => None,
            DivergingDiff::DiffBetween => {
                Self::write_meta_diff_if_present(
                    diff_tool,
                    &cr.crate_name,
                    &self.output,
                    &upstream_out,
                    &local_out,
                )
                .await
            }
        };

        if cr.diverging_diff.diverged()
            || !skip_non_diverging_diffs
            || pre_errors < self.num_local_failures + self.num_upstream_failures
        {
            self.crate_reports.push(CrateReport::new(
                cr.crate_name.clone(),
                cr.local_root.display().to_string(),
                cr.crate_url,
                cr.head_branch,
                cr.diverging_diff.diverged(),
                similar_errors,
                meta_diff_file,
                upstream_out,
                local_out,
            ));
        }
    }

    async fn write_meta_diff_if_present(
        diff_tool: Option<&Path>,
        crate_name: &CrateName,
        output_dirs: &OutputDirs,
        upstream_out: &FmtOutput,
        local_out: &FmtOutput,
    ) -> Option<PathBuf> {
        let content = match (
            upstream_out.diff_output_file.as_deref(),
            local_out.diff_output_file.as_deref(),
        ) {
            (Some(upstream), Some(local)) => match try_diff(diff_tool, upstream, local).await {
                DiffResult::Diff(d) => d,
                DiffResult::ToolNotFound => {
                    return None;
                }
                DiffResult::Error(e) => {
                    tracing::error!(
                        "failed to produce meta diff with diff_tool={:?}: {}",
                        diff_tool,
                        unpack(&*e)
                    );
                    return None;
                }
            },
            (a, b) => {
                tracing::error!(
                    "tried to run meta diff, but both upstream and local diffs were not present. upstream={:?}, local={:?}",
                    a,
                    b
                );
                return None;
            }
        };
        let name = match crate_name.try_convert_to_diverge_file_name() {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(
                    "failed to convert crate name to diverge file name: {}",
                    unpack(&*e)
                );
                return None;
            }
        };
        let path = place_file(output_dirs, &name, true, false);
        if let Err(e) = dump_content(&path, &content).await {
            tracing::error!(
                "failed to write diverge meta diff to path={}: {}",
                path.display(),
                unpack(&*e)
            );
            return None;
        }
        Some(path)
    }

    pub(crate) async fn finish_report(
        mut self,
        report_dest: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        self.crate_reports
            .sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
        tokio::task::spawn_blocking(move || {
            let path = if let Some(report_dest) = report_dest {
                report_dest
            } else {
                self.output.base.join("report.json")
            };
            let mut writer = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&path)
                .with_context(|| {
                    format!(
                        "failed to open report file for writing at {}",
                        path.display()
                    )
                })?;
            serde_json::to_writer_pretty(&mut writer, &self)
                .with_context(|| format!("failed to write report to {}", path.display()))?;
            if self.num_diverging_diffs > 0 {
                tracing::info!("Found {} diverging diffs", self.num_diverging_diffs);
            } else {
                tracing::info!("Found no diverging diffs");
            }
            tracing::info!("Wrote report to {}", path.display());
            self.html_report()?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("failed to join report writing task")??;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn create_rustfmt_output(
    crate_name: &CrateName,
    output: &OutputDirs,
    label: &'static str,
    write_outputs: bool,
    diverged: bool,
    analysis: RustfmtAnalysis,
    success_counter: &mut usize,
    diff_counter: &mut usize,
    failure_counter: &mut usize,
) -> FmtOutput {
    if analysis.rustfmt_error.is_none() && analysis.diff_output.is_none() {
        *success_counter += 1;
    }
    let diff_output_file = if let Some(diff) = analysis.diff_output {
        *diff_counter += 1;
        let file_name = crate_name.try_convert_to_diff_file_name(label);
        if write_outputs && let Ok(file_name) = file_name {
            let file_name = place_file(output, &file_name, diverged, false);
            if let Err(e) = dump_content(&file_name, &diff).await {
                tracing::error!("failed to dump diff output: {}", unpack(&*e));
                None
            } else {
                Some(file_name)
            }
        } else {
            None
        }
    } else {
        None
    };
    let error_output_file = if let Some(e) = analysis.rustfmt_error {
        *failure_counter += 1;
        let file_name = crate_name.try_convert_to_rustfmt_error_file_name(label);
        if write_outputs && let Ok(file_name) = file_name {
            let file_name = place_file(output, &file_name, diverged, true);
            if let Err(e) = dump_content(&file_name, &unpack(&*e).to_string()).await {
                tracing::error!("failed to dump error output: {}", unpack(&*e));
                None
            } else {
                Some(file_name)
            }
        } else {
            None
        }
    } else {
        None
    };
    FmtOutput {
        diff_output_file,
        error_output_file,
        elapsed: fmt_elapsed(analysis.elapsed),
    }
}

// Too many bools here
async fn dump_content(dest: &Path, content: &str) -> anyhow::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(&dest)
        .await
        .with_context(|| format!("failed to open {}", dest.display()))?;
    file.write_all(content.as_bytes())
        .await
        .with_context(|| format!("failed to write to {}", dest.display()))
}

fn place_file(output: &OutputDirs, file_name: &NormalPath, diverged: bool, err: bool) -> PathBuf {
    if err {
        output.errors.as_path().join(file_name.0.as_path())
    } else if diverged {
        output.diverged.as_path().join(file_name.0.as_path())
    } else {
        output.nondiverged.as_path().join(file_name.0.as_path())
    }
}

fn fmt_elapsed(elapsed: Duration) -> String {
    format!("{:.2}s", elapsed.as_secs_f64())
}

#[derive(serde::Serialize, Eq, PartialEq)]
struct CrateReport {
    crate_name: CrateName,
    local_root: String,
    repo_url: GitRepo,
    head_branch: String,
    diverged: bool,
    similar_errors: bool,
    meta_diff_file: Option<PathBuf>,
    upstream_rustfmt_output: FmtOutput,
    local_rustfmt_output: FmtOutput,
}

impl CrateReport {
    #[allow(clippy::too_many_arguments)]
    fn new(
        crate_name: CrateName,
        local_root: String,
        repo_url: GitRepo,
        head_branch: String,
        diverged: bool,
        similar_errors: bool,
        meta_diff_file: Option<PathBuf>,
        upstream_rustfmt_output: FmtOutput,
        local_rustfmt_output: FmtOutput,
    ) -> Self {
        Self {
            crate_name,
            local_root,
            repo_url,
            head_branch,
            diverged,
            similar_errors,
            meta_diff_file,
            upstream_rustfmt_output,
            local_rustfmt_output,
        }
    }

    fn has_error(&self) -> bool {
        self.upstream_rustfmt_output.error_output_file.is_some()
            || self.local_rustfmt_output.error_output_file.is_some()
    }

    fn has_diff(&self) -> bool {
        self.upstream_rustfmt_output.diff_output_file.is_some()
            || self.local_rustfmt_output.diff_output_file.is_some()
    }
}

#[derive(serde::Serialize, Eq, PartialEq)]
struct FmtOutput {
    diff_output_file: Option<PathBuf>,
    error_output_file: Option<PathBuf>,
    elapsed: String,
}

pub(crate) struct CrateAnalysis {
    pub(super) crate_name: CrateName,
    pub(super) local_root: PathBuf,
    pub(super) crate_url: GitRepo,
    pub(super) head_branch: String,
    pub(super) diverging_diff: DivergingDiff,
    pub(super) upstream_rustfmt_analysis: RustfmtAnalysis,
    pub(super) local_rustfmt_analysis: RustfmtAnalysis,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) enum DivergingDiff {
    LocalOnly,
    UpstreamOnly,
    DiffBetween,
    None,
}

impl DivergingDiff {
    #[inline]
    pub(crate) fn diverged(self) -> bool {
        self != Self::None
    }
}

impl CrateAnalysis {
    pub(super) fn new(
        crate_name: CrateName,
        local_root: PathBuf,
        crate_url: GitRepo,
        head_branch: String,
        diverging_diff: DivergingDiff,
        upstream_rustfmt_analysis: RustfmtAnalysis,
        local_rustfmt_analysis: RustfmtAnalysis,
    ) -> Self {
        Self {
            crate_name,
            local_root,
            crate_url,
            head_branch,
            diverging_diff,
            upstream_rustfmt_analysis,
            local_rustfmt_analysis,
        }
    }
}

pub(super) struct RustfmtAnalysis {
    pub(super) diff_output: Option<String>,
    pub(super) rustfmt_error: Option<anyhow::Error>,
    pub(super) elapsed: Duration,
}
