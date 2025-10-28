use crate::fs::Workdir;
use dashmap::DashSet;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

mod analyze;
pub(crate) mod cmd;
mod crates;
pub(crate) mod error;
mod fs;
mod git;
mod local_crates;
mod sync;

pub use crate::analyze::AnalyzeArgs;
use crate::analyze::report::{AnalysisReport, CrateAnalysis};
use crate::cmd::{RustFmtBuildOutputs, build_rustfmt};
use crate::crates::crate_consumer::default::PrunedCrate;
use crate::git::CrateReadyForAnalysis;
pub use crate::sync::{StopReceiver, stop_channel};
pub use crates::crate_consumer::default::ConsumerOpts;
pub use error::unpack;

pub struct MeteroidConfig {
    pub workdir: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub consumer_opts: ConsumerOpts,
    pub crate_source: CrateSource,
    pub analyze_args: AnalyzeArgs,
    pub analysis_max_concurrent: NonZeroUsize,
    pub analysis_timeout: Duration,
    pub stop_receiver: StopReceiver,
}

pub enum CrateSource {
    GitSync(GitSyncConfig),
    LocalCrates(LocalCratesConfig),
}

pub struct GitSyncConfig {
    pub crates_index_max_age_days: u8,
    pub git_resync_before: bool,
    pub git_clone_max_concurrent: NonZeroUsize,
}

pub struct LocalCratesConfig {
    pub crate_dir: PathBuf,
}

#[inline]
pub async fn meteoroid(config: MeteroidConfig) -> anyhow::Result<()> {
    exec_parallel(config).await
}

async fn exec_parallel(mut config: MeteroidConfig) -> anyhow::Result<()> {
    let wd = Workdir::new(config.workdir);
    let (sync_stop_send, sync_stop_recv) = stop_channel();
    let (sync, local_build_outputs, upstream_build_outputs) = match config.crate_source {
        CrateSource::GitSync(gs) => {
            let Some((local_build_outputs, upstream_build_outputs, targets)) = config
                .stop_receiver
                .with_stop(prepare_rustfmt_and_fetched_crates(
                    &wd,
                    config.analyze_args.rustfmt_repo,
                    config.analyze_args.rustfmt_upstream_repo,
                    gs.crates_index_max_age_days,
                    config.consumer_opts,
                ))
                .await
                .transpose()?
            else {
                tracing::info!("stopped before starting analysis, exiting");
                return Ok(());
            };
            let sync = git::run_sync_task(
                wd,
                gs.git_resync_before,
                targets,
                gs.git_clone_max_concurrent,
                sync_stop_recv,
            );
            (sync, local_build_outputs, upstream_build_outputs)
        }
        CrateSource::LocalCrates(lc) => {
            let Some((local_build_outputs, upstream_build_outputs)) = config
                .stop_receiver
                .with_stop(prepare_rustfmt(
                    config.analyze_args.rustfmt_repo,
                    config.analyze_args.rustfmt_upstream_repo,
                ))
                .await
                .transpose()?
            else {
                tracing::info!("stopped before starting analysis, exiting");
                return Ok(());
            };
            let sync = local_crates::local_crate_find_task(
                lc.crate_dir,
                config.analysis_max_concurrent,
                config.consumer_opts,
                sync_stop_recv,
            );
            (sync, local_build_outputs, upstream_build_outputs)
        }
    };
    let (analysis_out_send, analysis_out_recv) = tokio::sync::mpsc::channel(32);

    let (analysis_stop_send, mut analysis_stop_recv) = stop_channel();
    tokio::task::spawn(async move {
        match analysis_stop_recv
            .with_stop(analysis_task(
                sync,
                analysis_out_send,
                local_build_outputs,
                upstream_build_outputs,
                config.analyze_args.config,
                config.analysis_max_concurrent,
                config.analysis_timeout,
            ))
            .await
        {
            None => {
                tracing::info!("analysis task was stopped before finishing, exiting");
            }
            Some(()) => {
                tracing::debug!("analysis task finished");
            }
        }
    });

    let mut report = AnalysisReport::new(config.output_dir).await?;

    match config
        .stop_receiver
        .with_stop(drain_analyses(
            analysis_out_recv,
            &mut report,
            config.analyze_args.write_outputs,
            config.analyze_args.skip_non_diverging_diffs,
            config.analyze_args.diff_tool.as_deref(),
        ))
        .await
    {
        None => {
            tracing::info!("analysis task was stopped before finishing, gracefully exiting");
        }
        Some(()) => {
            tracing::debug!("analysis drain finished");
        }
    }
    report
        .finish_report(config.analyze_args.report_dest)
        .await?;
    sync_stop_send.stop().await;
    analysis_stop_send.stop().await;
    Ok(())
}

async fn drain_analyses(
    mut analysis_out_recv: tokio::sync::mpsc::Receiver<CrateAnalysis>,
    report: &mut AnalysisReport,
    write_outputs: bool,
    skip_non_diverging_diffs: bool,
    diff_tool: Option<&Path>,
) {
    while let Some(next) = analysis_out_recv.recv().await {
        report
            .add_result(diff_tool, next, write_outputs, skip_non_diverging_diffs)
            .await;
    }
}

async fn prepare_rustfmt_and_fetched_crates(
    workdir: &Workdir,
    rustfmt_repo: PathBuf,
    rustfmt_upstream_repo: PathBuf,
    crates_index_max_age_days: u8,
    consumer_opts: ConsumerOpts,
) -> anyhow::Result<(RustFmtBuildOutputs, RustFmtBuildOutputs, Vec<PrunedCrate>)> {
    let build_task = build_sequential(rustfmt_repo, rustfmt_upstream_repo);
    let ((local_build_outputs, upstream_build_outputs), targets) = tokio::try_join!(
        build_task,
        fetch_and_process_crates(workdir, crates_index_max_age_days, consumer_opts)
    )?;
    Ok((local_build_outputs, upstream_build_outputs, targets))
}

async fn prepare_rustfmt(
    rustfmt_repo: PathBuf,
    rustfmt_upstream_repo: PathBuf,
) -> anyhow::Result<(RustFmtBuildOutputs, RustFmtBuildOutputs)> {
    let build_task = build_sequential(rustfmt_repo, rustfmt_upstream_repo).await?;
    Ok((build_task.0, build_task.1))
}

// If not built sequentially, there can be toolchain download raciness
async fn build_sequential(
    rustfmt_repo: PathBuf,
    rustfmt_upstream_repo: PathBuf,
) -> anyhow::Result<(RustFmtBuildOutputs, RustFmtBuildOutputs)> {
    let local_build_outputs = build_rustfmt(&rustfmt_repo).await?;
    let upstream_build_outputs = build_rustfmt(&rustfmt_upstream_repo).await?;
    Ok((local_build_outputs, upstream_build_outputs))
}

async fn fetch_and_process_crates(
    wd: &Workdir,
    crates_index_max_age_days: u8,
    consumer_opts: ConsumerOpts,
) -> anyhow::Result<Vec<PrunedCrate>> {
    wd.ensure_workdir().await?;
    if wd.needs_crates_refetch(crates_index_max_age_days).await? {
        crates::update_index_to(&wd.base).await?;
    }
    let mut consumer = crates::crate_consumer::default::Consumer::new(consumer_opts);
    crates::csv_parse::consume_crates_data(wd, &mut consumer)?;
    Ok(consumer.get_crates())
}

#[allow(clippy::too_many_arguments)]
async fn analysis_task(
    mut recv: tokio::sync::mpsc::Receiver<CrateReadyForAnalysis>,
    send: tokio::sync::mpsc::Sender<CrateAnalysis>,
    local_build_outputs: RustFmtBuildOutputs,
    upstream_build_outputs: RustFmtBuildOutputs,
    config: Option<String>,
    max_concurrent: NonZeroUsize,
    timeout: Duration,
) {
    let mut unordered = FuturesUnordered::new();
    let seen = Arc::new(DashSet::default());
    while let Some(next) = recv.recv().await {
        let rr = local_build_outputs.clone();
        let upstream_rr = upstream_build_outputs.clone();
        let seen_c = seen.clone();
        let cfg_c = config.clone();
        unordered.push(tokio::task::spawn(async move {
            analyze::analyze_crate(&next, &rr, &upstream_rr, cfg_c.as_deref(), seen_c, timeout)
                .await
        }));
        if unordered.len() >= max_concurrent.get() {
            let Some(next) = unordered.next().await else {
                tracing::error!("analysis task was empty, this should never happen");
                continue;
            };
            on_analysis(next, &send).await;
        }
    }
    while let Some(res) = unordered.next().await {
        on_analysis(res, &send).await;
    }
}

async fn on_analysis(
    value: Result<anyhow::Result<Option<CrateAnalysis>>, tokio::task::JoinError>,
    send: &tokio::sync::mpsc::Sender<CrateAnalysis>,
) {
    match value {
        Ok(Ok(Some(res))) => {
            if send.send(res).await.is_err() {
                tracing::error!("analysis task sender was dropped, exiting");
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => {
            tracing::error!("analysis task failed: {}", unpack(&*e));
        }
        Err(e) => {
            tracing::error!("analysis task join failed: {}", unpack(&e));
        }
    }
}
