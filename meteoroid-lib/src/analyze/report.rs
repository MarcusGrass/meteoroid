use crate::crates::crate_consumer::default::{CrateName, GitRepo, NormalPath};
use crate::unpack;
use anyhow::Context;
use std::path::PathBuf;
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
        cr: CrateAnalysis,
        write_outputs: bool,
        include_non_diverging: bool,
    ) {
        let mut rep = CrateReport::new(
            cr.crate_name.clone(),
            cr.local_root.display().to_string(),
            cr.crate_url,
            cr.head_branch,
        );
        if cr.diverving_diff {
            self.num_diverging_diffs += 1;
        }
        let failures_pre = self.num_upstream_failures + self.num_local_failures;
        if let Some(upstream) = cr.upstream_rustfmt_analysis {
            let out = create_rustfmt_output(
                &cr.crate_name,
                &self.output,
                "upstream",
                write_outputs,
                cr.diverving_diff,
                upstream,
                &mut self.num_upstream_successes,
                &mut self.num_upstream_diffs,
                &mut self.num_upstream_failures,
            )
            .await;
            rep.upstream_rustfmt_output = Some(out);
        }
        if let Some(local) = cr.local_rustfmt_analysis {
            let out = create_rustfmt_output(
                &cr.crate_name,
                &self.output,
                "local",
                write_outputs,
                cr.diverving_diff,
                local,
                &mut self.num_local_successes,
                &mut self.num_local_diffs,
                &mut self.num_local_failures,
            )
            .await;
            rep.local_rustfmt_output = Some(out);
        }
        // Write if include, or has diff, or there's an error
        if include_non_diverging
            || cr.diverving_diff
            || failures_pre < self.num_local_failures + self.num_upstream_failures
        {
            self.crate_reports.push(rep);
        }
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
            if let Err(e) = dump_content(output, &file_name, &diff, diverged, false).await {
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
            if let Err(e) =
                dump_content(output, &file_name, &unpack(&*e).to_string(), diverged, true).await
            {
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
        diff_output_file: diff_output_file.map(|f| f.0.display().to_string()),
        error_output_file: error_output_file.map(|f| f.0.display().to_string()),
        elapsed: fmt_elapsed(analysis.elapsed),
    }
}

// Too many bools here
async fn dump_content(
    output: &OutputDirs,
    file_name: &NormalPath,
    content: &str,
    diverged: bool,
    err: bool,
) -> anyhow::Result<()> {
    let path = if err {
        output.errors.as_path().join(file_name.0.as_path())
    } else if diverged {
        output.diverged.as_path().join(file_name.0.as_path())
    } else {
        output.nondiverged.as_path().join(file_name.0.as_path())
    };
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(&path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(content.as_bytes())
        .await
        .with_context(|| format!("failed to write to {}", path.display()))
}

fn fmt_elapsed(elapsed: Duration) -> String {
    format!("{:.2}s", elapsed.as_secs_f64())
}

#[derive(serde::Serialize)]
struct CrateReport {
    crate_name: CrateName,
    local_root: String,
    repo_url: GitRepo,
    head_branch: String,
    check_output: Option<CheckOutput>,
    upstream_rustfmt_output: Option<FmtOutput>,
    local_rustfmt_output: Option<FmtOutput>,
}

impl CrateReport {
    fn new(
        crate_name: CrateName,
        local_root: String,
        repo_url: GitRepo,
        head_branch: String,
    ) -> Self {
        Self {
            crate_name,
            local_root,
            repo_url,
            head_branch,
            check_output: None,
            upstream_rustfmt_output: None,
            local_rustfmt_output: None,
        }
    }
}

#[derive(serde::Serialize)]
struct FmtOutput {
    diff_output_file: Option<String>,
    error_output_file: Option<String>,
    elapsed: String,
}

#[derive(serde::Serialize)]
struct CheckOutput {
    error_output_file: Option<String>,
    elapsed: String,
}

pub(crate) struct CrateAnalysis {
    pub(super) crate_name: CrateName,
    pub(super) local_root: PathBuf,
    pub(super) crate_url: GitRepo,
    pub(super) head_branch: String,
    pub(super) diverving_diff: bool,
    pub(super) upstream_rustfmt_analysis: Option<RustfmtAnalysis>,
    pub(super) local_rustfmt_analysis: Option<RustfmtAnalysis>,
}

impl CrateAnalysis {
    pub(super) fn new(
        crate_name: CrateName,
        local_root: PathBuf,
        crate_url: GitRepo,
        head_branch: String,
    ) -> Self {
        Self {
            crate_name,
            local_root,
            crate_url,
            head_branch,
            diverving_diff: false,
            upstream_rustfmt_analysis: None,
            local_rustfmt_analysis: None,
        }
    }
}

pub(super) struct RustfmtAnalysis {
    pub(super) diff_output: Option<String>,
    pub(super) rustfmt_error: Option<anyhow::Error>,
    pub(super) elapsed: Duration,
}
