use clap::Parser;
use meteoroid_lib::{AnalyzeArgs, ConsumerOpts, MeteroidConfig, stop_channel, unpack};
use std::marker::PhantomData;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::process::ExitCode;
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, Filter, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Debug, clap::Parser)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Path to the working directory for meteoroid
    /// This is where the crates index is downloaded to, where crates are cloned into, etc.
    /// It works as a cache as well as a place to store the output files
    #[clap(long, short)]
    workdir: PathBuf,
    /// Path to where analysis results are stored.
    /// Diff files, complete error outputs, and the run-report
    /// If unset, a temporary directory will be used
    #[clap(long, short)]
    output_dir: Option<PathBuf>,
    /// Path to the local/modified rustfmt repository that should be tested
    #[clap(long)]
    rustfmt_local_repo: PathBuf,
    /// Path to the unmodified rustfmt repository that should be used as a baseline
    #[clap(long)]
    rustfmt_upstream_repo: PathBuf,
    /// How old the cached crates index is allowed to be before a new database dump is fetched.
    #[clap(long, short, default_value_t = 7)]
    crates_index_max_age: u8,
    /// Whether to resync previously cloned crates before running analysis
    #[clap(long, short, default_value_t = false)]
    git_resync_before: bool,
    /// The number of git-clones (or refetches) that are allowed to run concurrently
    #[clap(long, default_value = "2")]
    git_sync_max_concurrent: NonZeroUsize,
    /// The maximum amount of crates to pull
    #[clap(long, default_value_t = 100)]
    max_crates: usize,
    /// The minimum size of a crate to be pulled
    #[clap(long, default_value_t = 20_000)]
    min_size: u64,
    /// Exclude crates that contains strings supplied here
    #[clap(long)]
    exclude_crate_name_contains: Vec<String>,
    /// Exclude repositories that contains strings supplied here
    #[clap(long)]
    exclude_repository_contains: Vec<String>,
    /// Don't output any files (except the report)
    #[clap(long, default_value_t = false)]
    no_output_files: bool,
    /// Where to output the report (defaults to `output-dir`)
    #[clap(long)]
    report_dest: Option<PathBuf>,
    /// Include non diverging crate details in the report (may create significant noise)
    /// statistics for all analyzed crates are included either way
    #[clap(long)]
    report_non_diverging: bool,
    /// Maximum crates to analyze concurrently,
    /// defaults to available parallelism (usually the number of cores),
    /// if that is unavailable `2` will be used
    #[clap(long)]
    analysis_max_concurrent: Option<NonZeroUsize>,
    /// How long to maximally wait for a `rustfmt` process to finish once started.
    #[clap(long, default_value = "30")]
    analysis_task_timeout_seconds: NonZeroU32,
    /// Extra command-line `config` variables, passed directly to `rustfmt`
    #[clap(long)]
    config: Option<String>,
    /// The verbosity of this tool,
    /// - `0` is no output except errors
    /// - `1` is low verbosity, `info` and more severe
    /// - `2` is normal verbosity, `debug` and more severe
    /// - `3` is unrestricted verbosity, `trace` and up
    #[clap(long, short, default_value_t = 2)]
    verbosity: u8,
}

#[tokio::main]
async fn main() -> ExitCode {
    const TWO: NonZeroUsize = NonZeroUsize::new(2).unwrap();
    let args = Args::parse();
    match args.verbosity {
        0 => setup_tracing::<VerbosityNone>(),
        1 => setup_tracing::<VerbosityLow>(),
        2 => setup_tracing::<VerbosityNormal>(),
        3 => setup_tracing::<VerbosityVery>(),
        unk => {
            eprintln!("unrecognized verbosity level: {unk}");
            return ExitCode::FAILURE;
        }
    }
    let num_parallel = args
        .analysis_max_concurrent
        .unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(TWO));
    let opts = ConsumerOpts {
        min_size: args.min_size,
        max_crates: args.max_crates,
        exclude_crate_name_contains: args.exclude_crate_name_contains,
        exclude_repository_contains: args.exclude_repository_contains,
    };
    let (stop_send, stop_recv) = stop_channel();
    let config = MeteroidConfig {
        workdir: args.workdir,
        output_dir: args.output_dir,
        crates_index_max_age_days: args.crates_index_max_age,
        git_resync_before: args.git_resync_before,
        git_clone_max_concurrent: args.git_sync_max_concurrent,
        consumer_opts: opts,
        analyze_args: AnalyzeArgs {
            rustfmt_repo: args.rustfmt_local_repo,
            rustfmt_upstream_repo: args.rustfmt_upstream_repo,
            report_dest: args.report_dest,
            config: args.config,
            write_outputs: !args.no_output_files,
            include_non_diverging_crates: args.report_non_diverging,
        },
        analysis_max_concurrent: num_parallel,
        analysis_timeout: std::time::Duration::from_secs(u64::from(
            args.analysis_task_timeout_seconds.get(),
        )),
        stop_receiver: stop_recv,
    };
    let mut meteoroid_task = tokio::task::spawn(meteoroid_lib::meteoroid(config));
    let mut stop_send = Some(stop_send);

    loop {
        tokio::select! {
            lib_res = &mut meteoroid_task => {
                match lib_res {
                    Ok(Ok(())) => {
                        tracing::info!("meteoroid run completed");
                        break ExitCode::SUCCESS;
                    }
                    Ok(Err(e)) => {
                        eprintln!("meteoroid run failed: {}", unpack(&*e));
                        break ExitCode::FAILURE;
                    }
                    Err(e) => {
                        eprintln!("meteoroid run failed, failed to join task: {}", unpack(&e));
                        break ExitCode::FAILURE;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                return if let Some(stop) = stop_send.take() {
                    eprintln!("received ctrl-c, attempting graceful shutdown, ctrl-c again to force stop");
                    tokio::task::spawn(stop.stop());
                    continue;
                } else {
                    eprintln!("received second ctrl-c, halting immediately");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn setup_tracing<V: VerbosityFilter>() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(LogFilter::<V>::new()))
        .init();
}

trait VerbosityFilter: Send + Sync + 'static {
    fn allow(level: Level) -> bool;
}

struct VerbosityVery;

impl VerbosityFilter for VerbosityVery {
    #[inline]
    fn allow(_level: Level) -> bool {
        true
    }
}

struct VerbosityNormal;

impl VerbosityFilter for VerbosityNormal {
    #[inline]
    fn allow(level: Level) -> bool {
        level <= Level::DEBUG
    }
}

struct VerbosityLow;

impl VerbosityFilter for VerbosityLow {
    #[inline]
    fn allow(level: Level) -> bool {
        level <= Level::INFO
    }
}

struct VerbosityNone;

impl VerbosityFilter for VerbosityNone {
    #[inline]
    fn allow(_level: Level) -> bool {
        false
    }
}

struct LogFilter<V>(PhantomData<V>);

impl<V> LogFilter<V> {
    fn new() -> Self {
        Self(PhantomData)
    }
}

impl<S, V> Filter<S> for LogFilter<V>
where
    S: Subscriber,
    V: VerbosityFilter,
{
    fn enabled(&self, meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        let tgt = meta.target();
        let (module, _ext) = if let Some((module, ext)) = tgt.split_once("::") {
            (module, ext)
        } else {
            (tgt, "")
        };
        if module == "meteoroid_lib" {
            return V::allow(*meta.level());
        }
        meta.level() < &Level::INFO
    }
}
