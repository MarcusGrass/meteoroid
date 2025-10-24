use crate::unpack;
use anyhow::{Context, bail};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct WorkspaceRoots {
    pub(crate) roots: Vec<PathBuf>,
}
pub(crate) async fn read_members(repo_root: &Path) -> anyhow::Result<Option<WorkspaceRoots>> {
    let c_toml = repo_root.join("Cargo.toml");
    if !tokio::fs::try_exists(&c_toml)
        .await
        .with_context(|| format!("failed to check if {} exists", c_toml.display()))?
    {
        return Ok(None);
    }
    let c_toml_raw = tokio::fs::read_to_string(&c_toml)
        .await
        .with_context(|| format!("failed to read Cargo.toml at {}", c_toml.display()))?;
    let members = crate::cargo::parse_members(&c_toml_raw)?;
    let mut roots = vec![];
    if members.is_empty() {
        roots.push(repo_root.to_path_buf());
        return Ok(Some(WorkspaceRoots { roots }));
    }
    for member in members {
        let member_path = repo_root.join(member);
        let unpacked = crate::cargo::unpack_member(member_path).await?;
        roots.extend(unpacked);
    }
    Ok(Some(WorkspaceRoots { roots }))
}

fn parse_members(raw: &str) -> anyhow::Result<Vec<String>> {
    let ct = cargo_toml::Manifest::from_str(raw).context("failed to parse Cargo.toml")?;
    let Some(ws) = ct.workspace else {
        return Ok(vec![]);
    };
    if !ws.default_members.is_empty() {
        return Ok(ws.default_members);
    }
    if ws.members.is_empty() {
        return Ok(vec![]);
    }
    Ok(ws.members)
}

async fn unpack_member(mut member: PathBuf) -> anyhow::Result<Vec<PathBuf>> {
    let mut unpacked = vec![];
    if !member.ends_with("*") {
        unpacked.push(member);
        return Ok(unpacked);
    }
    member.pop();
    let mut rd = tokio::fs::read_dir(&member).await.with_context(|| {
        format!(
            "failed to follow wildcard membder dir: {}",
            member.display()
        )
    })?;
    loop {
        let ent = match rd.next_entry().await {
            Ok(Some(ent)) => ent,
            Ok(None) => break,
            Err(e) => {
                bail!(
                    "failed to read next entry from wildcard member at {}: {}",
                    member.display(),
                    unpack(&e)
                );
            }
        };
        let md = ent.metadata().await.with_context(|| {
            format!("failed to get metadata for member at {}", member.display())
        })?;
        if !md.is_dir() {
            tracing::trace!(
                "found non-directory in wildcard member at {} when looking for sub-crates: {}",
                member.display(),
                ent.path().display()
            );
            continue;
        }
        unpacked.push(ent.path());
    }
    Ok(unpacked)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[tokio::test]
    async fn parse_cargo_toml() {
        let path = Path::new("../");
        let roots = crate::cargo::read_members(path).await.unwrap().unwrap();
        assert_eq!(roots.roots.len(), 2);
    }
}
