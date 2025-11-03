pub(crate) mod api;
pub(crate) mod crate_consumer;
pub(crate) mod csv_parse;

use crate::error::unpack;
use anyhow::Context;
use futures::StreamExt;
use reqwest::Response;
use std::path::{Path, PathBuf};
use std::sync::mpsc::TrySendError;

pub(crate) async fn update_index_to(path: &Path) -> anyhow::Result<()> {
    const TAR_URL: &str = "https://static.crates.io/db-dump.tar.gz";
    let client = reqwest::Client::builder()
        .user_agent("meteoroid-marcus.grass@protonmail.com")
        .use_rustls_tls()
        .build()
        .context("failed to build reqwest client")?;
    tracing::debug!("fetching crates index tar from {}", TAR_URL);
    let resp = client
        .get(TAR_URL)
        .send()
        .await
        .with_context(|| format!("failed to fetch crates index tar from {TAR_URL}"))?;
    let resp = resp
        .error_for_status()
        .context("failed to fetch crates index tar")?;
    tracing::debug!(
        "got success response from {}, starting stream decode",
        TAR_URL
    );
    let reader = response_reader(resp);
    untar_gzipped(reader, path.to_path_buf()).await?;
    Ok(())
}

fn response_reader(response: Response) -> AsyncReadShim {
    let (send, recv) = std::sync::mpsc::sync_channel(32);
    tokio::task::spawn(async move {
        let mut stream = response.bytes_stream();
        while let Some(next) = stream.next().await {
            let data = match next {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("failed to read from response stream: {}", unpack(&e));
                    break;
                }
            };
            // This construction is not ideal, timed poll ready on a sync channel
            loop {
                match send.try_send(data.to_vec()) {
                    Ok(()) => {
                        break;
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        tracing::debug!(
                            "tar response sender closed, aborting read (this could happen because it finished early)"
                        );
                        return;
                    }
                    Err(TrySendError::Full(_)) => {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
    });
    AsyncReadShim {
        recv,
        overflow: vec![],
    }
}

struct AsyncReadShim {
    recv: std::sync::mpsc::Receiver<Vec<u8>>,
    overflow: Vec<u8>,
}

impl std::io::Read for AsyncReadShim {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let overflow_len = self.overflow.len();
        let buf_len = buf.len();
        if overflow_len > 0 {
            return if buf_len >= overflow_len {
                buf[..overflow_len].copy_from_slice(&self.overflow);
                self.overflow = vec![];
                Ok(overflow_len)
            } else {
                let rem = overflow_len - buf_len;
                buf.copy_from_slice(&self.overflow[..buf_len]);
                self.overflow.copy_within(buf_len.., 0);
                self.overflow.truncate(rem);
                Ok(buf_len)
            };
        }
        let data = self
            .recv
            .recv()
            .map_err(|_| std::io::Error::other("input channel closed"))?;
        if buf.len() >= data.len() {
            buf[..data.len()].copy_from_slice(&data);
            return Ok(data.len());
        }
        buf.copy_from_slice(&data[..buf_len]);
        self.overflow = data[buf_len..].to_vec();
        Ok(buf_len)
    }
}

async fn untar_gzipped<R: std::io::Read + Send + 'static>(
    mut reader: R,
    dest: PathBuf,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        let gz_decoder = flate2::read::GzDecoder::new(&mut reader);
        let mut tar = tar::Archive::new(gz_decoder);
        let entries = tar.entries().context("failed to read tar entries")?;
        let mut versions_unpacked = false;
        let mut crates_unpacked = false;
        for ent_res in entries {
            let mut ent = ent_res.context("failed to read tar entry")?;
            let ent_path = ent.path().context("failed to get tar entry path")?;
            if ent_path.ends_with("versions.csv") {
                let versions_dest = dest.join("versions.csv");
                ent.unpack(&versions_dest).with_context(|| {
                    format!("failed to unpack crates index tar at {}", dest.display())
                })?;
                tracing::debug!("unpacked versions.csv to {}", versions_dest.display());
                versions_unpacked = true;
            } else if ent_path.ends_with("crates.csv") {
                let crates_dest = dest.join("crates.csv");
                ent.unpack(&crates_dest).with_context(|| {
                    format!("failed to unpack crates index tar at {}", dest.display())
                })?;
                crates_unpacked = true;
                tracing::debug!("unpacked crates.csv to {}", crates_dest.display());
            }
            if versions_unpacked && crates_unpacked {
                tracing::debug!(
                    "unpacked all needed files from crates index tar to {}",
                    dest.display()
                );
                return Ok(());
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("failed to unpack crates index tar")??;
    Ok(())
}
