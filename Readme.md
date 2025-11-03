# Meteoroid, something that will cause a small crater

A tool made for testing changes to rustfmt on a locally runnable subset of popular crates on 
`crates.io`.

## Usage

### Prep

This tool currently requires `git`, `rustup` and `cargo` to be installed and 
available on path. It also requires at least a `unix-like` file handling scheme, 
although `Linux` is the only platform currently tested.

1. Make sure you have your repository ready for your modified (fork probably) version
of [`rustfmt`](https://github.com/rust-lang/rustfmt.git).
2. Clone a fresh separate copy of `rustfmt` to another directory, ex: `git clone git@github.com:rust-lang/rustfmt.git ./unmodified-rustfmt`
3. Ensure that your changes and the `master`-branch of `rustfmt` are in sync. This isn't a hard requirement, but
if you don't do this there's some risks that you get drift because of version-drift unrelated to your changes.
Additionally, this will potentially make you have to download another `nightly` toolchain (happens automatically at build).
4. Ensure that you have some directory to put output files like diffs, errors, report (not mandatory, will use temp otherwise).
5. Ensure that you have some working directory, this is where crates, git repos, and other metadata will be cached.
6. Run this project (see below).

Since this project analyzes untrusted code, using docker could potentially reduce risks some (although `rustfmt` 
currently does not execute build scripts).

There's a [Dockerfile](./Dockerfile) in the root that can be used to build and run the project

```shell
# Using the current user will help with build artifacts not switching permissions
docker build . --build-arg "USERID=$UID" --build-arg "GROUPID=$(id -g)" -t meteoroid
docker run -u $UID --rm \
--mount type=bind,src="<your-workdir>",dst=/data/workdir \
--mount type=bind,src="<your-local-modified-rustfmt-checkout>",dst=/data/local \
--mount type=bind,src="<your-upstream-rustfmt-checkout>",dst=/data/upstream \
--mount type=bind,src="<your-analysis-output-directory>",dst=/data/output \
--mount type=bind,src="./target",dst=/app/target \
meteoroid <extra args>
```

Or use the supplied script at [.local/docker.sh](./.local/docker.sh) (requires `bash`)

Running with remote fetch:

```shell
./.local/docker.sh <your-workdir> <your-local-modified-rustfmt-checkout> <your-upstream-rustfmt-checkout> <your-analysis-output-directory> <extra-args> remote <remote-args>
```

Running with local crates currently has a potentially bewildering UX where if running with 
remote fetch it requires the subcommand `remote` to run, while if local is specified with `METEOROID_LOCAL` 
no sub-command is required, but the script is only for convenience.

Local: 

```shell
METEOROID_LOCAL=~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ ./.local/docker.sh <your-workdir> <your-local-modified-rustfmt-checkout> <your-upstream-rustfmt-checkout> <your-analysis-output-directory> <extra-args>
```

One flaw with the container approach is that even if `target` is mounted, the crate index is re-fetched on each run.

Or run directly, using remote crate fetch:

```shell
cargo r -r -p meteoroid -- -w ~/output-dir/meteorite-data -o ~/output-dir/meteorite-results --rustfmt-local-repo ~/code/rustfmt/ --rustfmt-upstream-repo ~/code/upstream-rustfmt/ --max-crates 100 --analysis-max-concurrent $(nproc) remote --git-sync-max-concurrent 8 
```

Or locally using some directory containing crates, such as the local crates cache:

```shell
cargo r -r -p meteoroid -- -w ~/output-dir/meteorite-data -o ~/output-dir/meteorite-results --rustfmt-local-repo ~/code/rustfmt/ --rustfmt-upstream-repo ~/code/upstream-rustfmt/ --max-crates 100 --analysis-max-concurrent $(nproc) local -p ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/
```

`1000` crates takes about 15 minutes from a clean slate (on my machine with 16 git concurrent and 32 analysis concurrent), and leaves about `11G` of outputs in the workdir.

## How it works

1. Downloads crate-metadata from `crates.io`
2. Filter, keeping only creates with a repository link from `github` and `gitlab` (for simplicity, other forges can be added trivially, it's 
just a string-filter).
3. Sort by downloads
4. Clone repos to the supplied `workdir`
5. A tempdir is created (and logged as info up top) if no output directory is specified
6. Crate-by-crate (parallel), for workspace member in crate, collect all files for a workspace member, then run 
`rustfmt --check <files>`, first with the build from the supplied `rustfmt-upstream-repo`, which should be 
a clean checkout of `rustfmt`'s `master`-branch, then with the `rustfmt` from `rustfmt-local-repo`, which should be 
a checkout of the branch you want to test.
7. Every diff and error will be dumped as-is to a file under the output directory (that can be `cat`'ed for example), 
to see the specific difference. Diverging diffs go under the `diverged` subdirectory.
Diverging is defined as: Both upstream and local have diffs, but they aren't identical. Or: Only upstream has a diff.
Or: Only local has a diff.
8. It also outputs a bunch of logs, even at default verbosity, verbosity can be controlled with `-v`
9. In the end, it writes a `json`-file with a summation of the run
```json
{
  "num_check_failures": 0,
  "num_check_successes": 0,
  "num_upstream_failures": 0,
  "num_upstream_diffs": 0,
  "num_upstream_successes": 527,
  "num_local_failures": 0,
  "num_local_diffs": 0,
  "num_local_successes": 527,
  "crate_reports": [
    {
      ...
```
10. If `ctrl-c` is hit one, the application will try to exit gracefully and write a report to disk before finishing.
If hit twice, the application will try to immediately exit.

### Analyzing results

I have yet to figure out a good way of analyzing results at scale.
A simple check is looking under `output-dir/diverged`, any files there contains a diverging diff that can 
be examined to see if the change is acceptable. Pulling up all the diffs in one view and checking them off 
one-by-one would be helpful.

## Caveats

This is an extremely simple implementation, it works with some flaws, the biggest ones are:

### Git clone is a bit cumbersome

The alternative is to use `crates.io`'s API, but it's a bit sensitive and is limited to 1 request per second.
I don't want to overburden it. 
Additionally, using `git` gives context, so that we can pick up a `rustfmt.toml` in a workspace root for example.
Lastly, using `git` makes syncing easier. If the crate is downloaded as is, it's tricky to figure out if the current 
version is the latest without taxing `crates.io` with more requests. Updating if out-of-date is easier with git, 
but it has the caveat that it's the main branch that's checked out, not the latest published crate version.

#### Some repos are private

Some supplied repo links go to unusable repos, sometimes they're private and can't be cloned.

#### Some crates do not have any repo metadata

These are filtered out, so they're missed.

### Difficulty estimating exact number of crates

The max number of crates are post-filter candidates. The implementation checks out the crate's supplied repo, 
then runs `rustfmt` on all workspace members that it finds there (after parsing a `Cargo.toml` from the root). 

This means that it may run on more crates (if one workspace member is what triggered the clone, but no others are present).

### Untrusted input

The application operates on almost exclusively untrusted input. This isn't necessarily that big of an issue, 
since `rustfmt` to my knowledge does not run build-scripts or execute any code. But the code that's cloned is 
not vetted at all, 'random' repositories are cloned off the internet, then processed. That is risky in its nature.

This tool attempts to sanitize things that it uses as urls or paths, but it's impossible for me to guarantee that 
I have covered everything. Running in a container or VM could help.

## Future improvements

Some collected suggestions from `zulip`, and more.

### Use cratesync

Another suggestion for fetching crates is using [`cratesync`](https://github.com/m-ou-se/cratesync), 
this would be convenient, but at present it doesn't look rate-limited or have a user-agent set. 
Could be used after checking with `crates.io` that using it like that is alright. It fits the purpose well.

### Make platform independent

This should be possible, and it may already be platform independent.

### Make Github actions runnable

This would be pretty cool, but ideally caching would be solved so that multiple runs don't result in re-clones.

### Make a nice report

Could also be displayed in a Github actions run, but could just be a nicer way of analyzing outputs and diffs.