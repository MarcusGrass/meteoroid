use crate::crates::api::VersionsEntryBuilder;
use crate::crates::crate_consumer::CrateConsumer;
use crate::fs::Workdir;
use anyhow::Context;
use rustc_hash::FxHashMap;
use std::path::Path;

pub(crate) fn consume_crates_data(
    workdir: &Workdir,
    consumer: &mut impl CrateConsumer,
) -> anyhow::Result<()> {
    let name_id_mapping = parse_id_name_mapping(&workdir.crates_csv)?;
    parse_versions_xml(&workdir.versions_csv, &name_id_mapping, consumer)?;
    Ok(())
}

fn parse_versions_xml(
    path: &Path,
    name_id_mapping: &FxHashMap<u64, String>,
    consumer: &mut impl CrateConsumer,
) -> anyhow::Result<()> {
    tracing::debug!("parsing versions data from {}", path.display());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .create(false)
        .open(path)
        .with_context(|| format!("failed to open file at {}", path.display()))?;
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(file);
    let records = rdr.records();
    let mut records_read = 0;
    for rec_res in records {
        records_read += 1;
        let record = rec_res
            .with_context(|| format!("failed to read csv record from: {}", path.display()))?;
        let mut bldr = VersionsEntryBuilder::default();
        for val in &record {
            bldr.enter_next(val).with_context(|| {
                format!("failed to parse version entry from {}", path.display())
            })?;
        }
        let val = bldr.consume()?;
        let crate_name = name_id_mapping
            .get(&val.crate_id)
            .context("failed to find crate name for id")?;
        if !consumer.consume(crate_name, val)? {
            tracing::info!("consumer finished early, after {records_read} csv records read");
            break;
        }
    }
    tracing::debug!(
        "consumed {records_read} csv records from {}",
        path.display()
    );
    Ok(())
}

fn parse_id_name_mapping(path: &Path) -> anyhow::Result<FxHashMap<u64, String>> {
    tracing::debug!("parsing crate id to name mapping from {}", path.display());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .create(false)
        .open(path)
        .with_context(|| format!("failed to open file at {}", path.display()))?;
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(file);
    let records = rdr.records();
    let mut approx_size = 0;
    let mut map = FxHashMap::default();
    for rec_res in records {
        let record = rec_res
            .with_context(|| format!("failed to read csv record from: {}", path.display()))?;
        let id: u64 = record
            .get(4)
            .with_context(|| format!("no record at column 4 at {}", path.display()))?
            .parse()
            .with_context(|| format!("failed to parse id from csv record at {}", path.display()))?;
        let name: String = record
            .get(7)
            .with_context(|| format!("failed to parse name from csv record at {}", path.display()))?
            .to_string();
        approx_size += size_of::<u64>() + size_of::<String>() + name.len();
        map.insert(id, name);
    }
    tracing::debug!(
        "parsed {} crates id to name mappings with at approximate memory footprint of {approx_size}B from {}",
        map.len(),
        path.display()
    );
    Ok(map)
}

