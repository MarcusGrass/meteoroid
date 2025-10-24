pub(crate) mod default;

use crate::crates::api::VersionsEntry;

pub(crate) trait CrateConsumer {
    fn consume(&mut self, crate_name: &str, versions_entry: VersionsEntry) -> anyhow::Result<bool>;
}
