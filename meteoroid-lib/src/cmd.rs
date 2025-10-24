use crate::unpack;
use anyhow::{Context, bail};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

pub(crate) async fn output_string(cmd: &mut Command) -> anyhow::Result<String> {
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("failed to run command: {cmd:?}"))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(output.stdout.as_slice());
        let stderr = String::from_utf8_lossy(output.stderr.as_slice());
        anyhow::bail!("command failed: {cmd:?}\nstdout: {stdout:?}\nstderr: {stderr:?}");
    }
    Ok(String::from_utf8_lossy(output.stdout.as_slice()).to_string())
}

pub(crate) enum RustfmtOutput {
    Success,
    Diff(String),
    Failure(anyhow::Error),
}

pub(crate) async fn build_rustfmt(
    rustfmt_source_dir: &Path,
) -> anyhow::Result<RustFmtBuildOutputs> {
    let output = Command::new("cargo")
        .env_remove("RUSTUP_TOOLCHAIN")
        .arg("build")
        .arg("--release")
        .arg("--bin")
        .arg("rustfmt")
        .current_dir(rustfmt_source_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| {
            format!(
                "failed to build rustfmt in {}",
                rustfmt_source_dir.display()
            )
        })?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(output.stdout.as_slice());
        let stderr = String::from_utf8_lossy(output.stderr.as_slice());
        anyhow::bail!(
            "failed to build rustfmt in {}:\nstdout: {stdout:?}\nstderr: {stderr:?}",
            rustfmt_source_dir.display()
        );
    }
    let expected_built_binary = rustfmt_source_dir
        .join("target")
        .join("release")
        .join("rustfmt");
    if !tokio::fs::try_exists(&expected_built_binary)
        .await
        .with_context(|| {
            format!(
                "failed to check if {} exists",
                expected_built_binary.display()
            )
        })?
    {
        bail!(
            "expected rustfmt binary to be built at {}, but it does not exist there",
            expected_built_binary.display()
        );
    }
    let toolchain_lib_path = locate_rustfmt_toolchain(rustfmt_source_dir)
        .await
        .context("failed to locate toolchain lib path")?;
    tracing::info!(
        "built rustfmt binary at {} with LD_LIBRARY_PATH at {}",
        expected_built_binary.display(),
        toolchain_lib_path.0.display()
    );
    Ok(RustFmtBuildOutputs {
        built_binary_path: expected_built_binary,
        toolchain_lib_path,
    })
}

#[derive(Clone)]
pub struct RustFmtBuildOutputs {
    pub built_binary_path: PathBuf,
    pub toolchain_lib_path: ToolchainLibPath,
}

#[derive(Clone)]
pub struct ToolchainLibPath(PathBuf);

impl ToolchainLibPath {
    #[inline]
    pub(crate) fn ld_library_path(&self) -> &Path {
        &self.0
    }
}

async fn locate_rustfmt_toolchain(rustfmt_source_dir: &Path) -> anyhow::Result<ToolchainLibPath> {
    let output = Command::new("rustup")
        .env_remove("RUSTUP_TOOLCHAIN")
        .arg("show")
        .arg("active-toolchain")
        .current_dir(rustfmt_source_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| {
            format!(
                "failed to use rustup to check active toolchain in {}",
                rustfmt_source_dir.display()
            )
        })?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(output.stdout.as_slice());
        let stderr = String::from_utf8_lossy(output.stderr.as_slice());
        anyhow::bail!(
            "failed to check rustup toolchain in {}:\nstdout: {stdout:?}\nstderr: {stderr:?}",
            rustfmt_source_dir.display()
        );
    }
    let stdout = String::from_utf8_lossy(output.stdout.as_slice());
    let mut outputs = stdout.split(' ');
    let Some(toolchain) = outputs.next() else {
        anyhow::bail!(
            "failed to parse rustup output in {}, expected to find a toolchain:\nstdout: {stdout:?}",
            rustfmt_source_dir.display()
        );
    };
    let lib_dir = try_find_toolchain_lib_dir(toolchain).await?;
    Ok(ToolchainLibPath(lib_dir))
}

async fn try_find_toolchain_lib_dir(toolchain: &str) -> anyhow::Result<PathBuf> {
    if let Some(home_dir) = std::env::home_dir() {
        let home = PathBuf::from(&home_dir);
        let lib_dir = home
            .join(".rustup")
            .join("toolchains")
            .join(toolchain)
            .join("lib");
        tracing::debug!(
            "looking for toolchain: {toolchain} in {}",
            lib_dir.display()
        );
        if tokio::fs::try_exists(&lib_dir)
            .await
            .with_context(|| format!("failed to check if {} exists", lib_dir.display()))?
        {
            return Ok(lib_dir);
        }
        tracing::debug!(
            "failed to find toolchain: {toolchain} in {}",
            lib_dir.display()
        );
    }
    // If failed on home_dir, this will likely only work on Linux
    // And even within that, only some distros.
    // Used because this is how the rust debian docker image sets it up
    let toolchain_dir = Path::new("/")
        .join("usr")
        .join("local")
        .join("rustup")
        .join("toolchains")
        .join(toolchain)
        .join("lib");
    tracing::debug!(
        "looking for toolchain: {toolchain} in {}",
        toolchain_dir.display()
    );
    if tokio::fs::try_exists(&toolchain_dir)
        .await
        .with_context(|| format!("failed to check if {} exists", toolchain_dir.display()))?
    {
        return Ok(toolchain_dir);
    }
    bail!(
        "failed to find toolchain: {toolchain} in {} or under $HOME/.rustup/toolchains",
        toolchain_dir.display()
    );
}

pub(crate) async fn run_rustfmt(cmd: &mut Command, timeout: Duration) -> RustfmtOutput {
    let out = match tokio::time::timeout(
        timeout,
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return RustfmtOutput::Failure(anyhow::anyhow!(
                "command failed to finish: {}, cmd={cmd:?}",
                unpack(&e)
            ));
        }
        Err(_e) => {
            return RustfmtOutput::Failure(anyhow::anyhow!("command timed out, cmd={cmd:?}"));
        }
    };
    if out.status.success() {
        return RustfmtOutput::Success;
    }
    if let Some(1) = out.status.code() {
        if out.stdout.is_empty() {
            return RustfmtOutput::Failure(anyhow::anyhow!(
                "command failed: {cmd:?}\nstderr: {}",
                String::from_utf8_lossy(out.stderr.as_slice())
            ));
        }
        let stdout = String::from_utf8_lossy(out.stdout.as_slice()).to_string();
        return RustfmtOutput::Diff(stdout);
    }
    let stdout = String::from_utf8_lossy(out.stdout.as_slice());
    let stderr = String::from_utf8_lossy(out.stderr.as_slice());
    RustfmtOutput::Failure(anyhow::anyhow!(
        "command failed: {cmd:?}\nstdout: {stdout:?}\nstderr: {stderr:?}"
    ))
}
