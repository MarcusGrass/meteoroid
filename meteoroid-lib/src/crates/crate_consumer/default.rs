use crate::crates::api::VersionsEntry;
use crate::crates::crate_consumer::CrateConsumer;
use crate::unpack;
use anyhow::{Context, bail};
use rustc_hash::FxHashSet;
use std::collections::{BinaryHeap, HashSet};
use std::fmt::{Debug, Display, Formatter};
use std::path::{Component, PathBuf};
use url::Url;

pub struct ConsumerOpts {
    pub max_crates: usize,
    pub min_size: u64,
    pub exclude_crate_name_contains: Vec<String>,
    pub exclude_repository_contains: Vec<String>,
}

impl Default for ConsumerOpts {
    fn default() -> Self {
        Self {
            max_crates: 100,
            // Last time I checked, average was 177K
            min_size: 20_000,
            exclude_crate_name_contains: vec![],
            exclude_repository_contains: vec![],
        }
    }
}

impl ConsumerOpts {
    #[must_use]
    pub fn add_excluded_crate_name_contains(mut self, crate_name_contains: String) -> Self {
        self.exclude_crate_name_contains.push(crate_name_contains);
        self
    }
    #[must_use]
    pub fn add_excluded_repository_contains(mut self, repository_contains: String) -> Self {
        self.exclude_repository_contains.push(repository_contains);
        self
    }
}

#[derive(Debug)]
pub(crate) struct CrateByPopularity {
    downloads: u64,
    rt: RetainCrate,
}

impl PartialEq for CrateByPopularity {
    fn eq(&self, other: &Self) -> bool {
        self.downloads == other.downloads
    }
}

impl Eq for CrateByPopularity {}

#[allow(clippy::non_canonical_partial_ord_impl)]
impl PartialOrd for CrateByPopularity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(other.downloads.cmp(&self.downloads))
    }
}

impl Ord for CrateByPopularity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.downloads.cmp(&self.downloads)
    }
}

#[derive(Debug)]
pub(crate) struct RetainCrate {
    crate_name: CrateName,
    crate_id: u64,
    repository: GitRepo,
    repo_dir_name: RepoName,
}

#[derive(Default)]
pub(crate) struct Consumer {
    consumer_opts: ConsumerOpts,
    crates: BinaryHeap<CrateByPopularity>,
    contained_crate_ids: FxHashSet<u64>,
}

impl Consumer {
    pub fn new(consumer_opts: ConsumerOpts) -> Self {
        Self {
            consumer_opts,
            crates: BinaryHeap::new(),
            contained_crate_ids: HashSet::default(),
        }
    }
}

impl CrateConsumer for Consumer {
    fn consume(&mut self, crate_name: &str, versions_entry: VersionsEntry) -> anyhow::Result<bool> {
        if self.consumer_opts.min_size > versions_entry.crate_size {
            return Ok(true);
        }
        for excl in &self.consumer_opts.exclude_crate_name_contains {
            if crate_name.contains(excl) {
                return Ok(true);
            }
        }
        for excl in &self.consumer_opts.exclude_repository_contains {
            if versions_entry.repository.contains(excl) {
                return Ok(true);
            }
        }
        let (git_repo, repo_name) = match validate_repo(versions_entry.repository) {
            Ok((g, r)) => (g, r),
            Err(e) => {
                tracing::trace!(
                    "Rejected repository: '{}': {}",
                    versions_entry.repository,
                    unpack(&*e)
                );
                return Ok(true);
            }
        };
        if self.contained_crate_ids.contains(&versions_entry.crate_id) {
            return Ok(true);
        }
        let crate_name = match best_attempt_validate_path(crate_name) {
            Ok(cr) => cr,
            Err(e) => {
                tracing::trace!(
                    "rejected crate name for path validity: {crate_name}: {}",
                    unpack(&*e)
                );
                return Ok(true);
            }
        };
        if self.crates.len() >= self.consumer_opts.max_crates {
            let Some(cr) = self.crates.peek() else {
                bail!("crate length too long, but nothing to peek (this is a bug)");
            };
            if versions_entry.downloads > cr.downloads {
                let Some(cr) = self.crates.pop() else {
                    bail!("crate length too long, but nothing to pop (this is a bug)");
                };
                self.contained_crate_ids.remove(&cr.rt.crate_id);
                self.contained_crate_ids.insert(versions_entry.crate_id);
                self.crates.push(CrateByPopularity {
                    downloads: versions_entry.downloads,
                    rt: RetainCrate {
                        crate_name: CrateName(crate_name),
                        crate_id: versions_entry.crate_id,
                        repository: git_repo,
                        repo_dir_name: repo_name,
                    },
                });
            }
            Ok(true)
        } else {
            self.crates.push(CrateByPopularity {
                downloads: versions_entry.downloads,
                rt: RetainCrate {
                    crate_name: CrateName(crate_name),
                    crate_id: versions_entry.crate_id,
                    repository: git_repo,
                    repo_dir_name: repo_name,
                },
            });

            Ok(true)
        }
    }
}

/// Should be considered and treated as untrusted user input
#[derive(Debug, Clone, serde::Serialize, Eq, PartialEq, PartialOrd, Ord)]
pub(crate) struct CrateName(NormalPath);

impl CrateName {
    pub fn try_convert_to_diff_file_name(&self, label: &str) -> anyhow::Result<NormalPath> {
        let raw = format!("{}-{label}.diff", self.0.0.display());
        best_attempt_validate_path(&raw)
    }
    pub fn try_convert_to_rustfmt_error_file_name(
        &self,
        label: &str,
    ) -> anyhow::Result<NormalPath> {
        let raw = format!("{}-{label}-error.txt", self.0.0.display());
        best_attempt_validate_path(&raw)
    }
}

impl Display for CrateName {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}", self.0.0.display()))
    }
}

/// Should be considered and treated as untrusted user input
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GitRepo(Url);

impl GitRepo {
    #[inline]
    pub fn as_url(&self) -> &Url {
        &self.0
    }
}

impl Display for GitRepo {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

/// Should be considered and treated as untrusted user input
#[derive(Debug, Clone)]
pub(crate) struct RepoName(NormalPath);

impl RepoName {
    #[inline]
    pub fn as_path(&self) -> &std::path::Path {
        self.0.0.as_path()
    }
}

impl Display for RepoName {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}", self.0.0.display()))
    }
}

/// This function both validates that the repo is a valid url, and that the repo
/// can be turned into a path that **should** be valid.
/// Since `repository` is just metadata that's not validated, it is a potential attack
/// vector. This is a best-effort sanitation of what should be considered unsafe user input.
fn validate_repo(repo: &str) -> anyhow::Result<(GitRepo, RepoName)> {
    let url = Url::parse(repo).context("failed to parse repository url")?;
    if !url.scheme().starts_with("https") {
        bail!("url must be https");
    }
    let host = url.host_str().context("failed to get host")?;
    if host != "github.com" || host == "gitlab.com" {
        // Todo: Add more forges
        bail!("not a recognized forge: {host}");
    }
    let mut ps = url
        .path_segments()
        .context("failed to get path segments from repository url")?;
    let _org = ps.next().context("failed to get org from repository url")?;
    let repo_name = ps
        .next()
        .context("failed to get repo name from repository url")?;
    // Perhaps overly strict, but generally repos are <org>/<repo> in paths,
    if ps.next().is_some() {
        bail!("repository url has too many path segments");
    }
    let pb = best_attempt_validate_path(repo_name).context("failed to validate repository path")?;
    Ok((GitRepo(url), RepoName(pb)))
}

#[derive(Debug, Clone, serde::Serialize, Eq, PartialEq, PartialOrd, Ord)]
pub(crate) struct NormalPath(pub(crate) PathBuf);

fn best_attempt_validate_path(s: &str) -> anyhow::Result<NormalPath> {
    let pb = PathBuf::from(s);
    normalized_single(pb)
}

/// Waiting for [134694](https://github.com/rust-lang/rust/issues/134694)
fn normalized_single(path_buf: PathBuf) -> anyhow::Result<NormalPath> {
    let mut components = path_buf.components();
    let Some(first) = components.next() else {
        bail!("path {} contained no components", path_buf.display());
    };
    match first {
        Component::Normal(_n) => Ok(NormalPath(path_buf)),
        c => {
            bail!("unexpected component: {c:?}");
        }
    }
}

#[derive(Debug, Clone)]
pub struct PrunedCrate {
    pub(crate) crate_name: CrateName,
    pub(crate) repository: GitRepo,
    pub(crate) repo_dir_name: RepoName,
}

impl Consumer {
    pub(crate) fn get_crates(self) -> Vec<PrunedCrate> {
        self.crates
            .into_iter()
            .map(|c| PrunedCrate {
                crate_name: c.rt.crate_name,
                repository: c.rt.repository,
                repo_dir_name: c.rt.repo_dir_name,
            })
            .collect()
    }
}
