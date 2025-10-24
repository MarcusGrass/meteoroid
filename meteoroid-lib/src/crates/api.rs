use anyhow::{Context, bail};

#[derive(Debug, Default)]
pub(crate) struct VersionsEntry<'a> {
    pub(crate) bin_names: &'a str,
    pub(crate) categories: &'a str,
    pub(crate) checksum: &'a str,
    pub(crate) crate_id: u64,
    pub(crate) crate_size: u64,
    pub(crate) created_at: &'a str,
    pub(crate) description: &'a str,
    pub(crate) documentation: &'a str,
    pub(crate) downloads: u64,
    pub(crate) edition: &'a str,
    pub(crate) features: &'a str,
    pub(crate) has_lib: &'a str,
    pub(crate) homepage: &'a str,
    pub(crate) id: &'a str,
    pub(crate) keywords: &'a str,
    pub(crate) license: &'a str,
    pub(crate) links: &'a str,
    pub(crate) num: &'a str,
    pub(crate) num_no_build: &'a str,
    pub(crate) published_by: &'a str,
    pub(crate) repository: &'a str,
    pub(crate) rust_version: &'a str,
    pub(crate) updated_at: &'a str,
    pub(crate) yanked: bool,
}

#[derive(Default)]
pub(crate) struct VersionsEntryBuilder<'a> {
    inner: VersionsEntry<'a>,
    next_field: usize,
}

impl<'a> VersionsEntryBuilder<'a> {
    pub(crate) fn enter_next(&mut self, value: &'a str) -> anyhow::Result<bool> {
        match self.next_field {
            0 => self.inner.bin_names = value,
            1 => self.inner.categories = value,
            2 => self.inner.checksum = value,
            3 => self.inner.crate_id = value.parse().context("failed to parse crate id as u64")?,
            4 => {
                self.inner.crate_size =
                    value.parse().context("failed to parse crate size as u64")?;
            }
            5 => self.inner.created_at = value,
            6 => self.inner.description = value,
            7 => self.inner.documentation = value,
            8 => {
                self.inner.downloads = value.parse().context("failed to parse downloads as u64")?;
            }
            9 => self.inner.edition = value,
            10 => self.inner.features = value,
            11 => self.inner.has_lib = value,
            12 => self.inner.homepage = value,
            13 => self.inner.id = value,
            14 => self.inner.keywords = value,
            15 => self.inner.license = value,
            16 => self.inner.links = value,
            17 => self.inner.num = value,
            18 => self.inner.num_no_build = value,
            19 => self.inner.published_by = value,
            20 => self.inner.repository = value,
            21 => self.inner.rust_version = value,
            22 => self.inner.updated_at = value,
            23 => self.inner.yanked = parse_yanked_bool(value)?,
            overflow => {
                bail!("too many fields entered in version entry builder: {overflow}");
            }
        }
        self.next_field += 1;
        Ok(self.next_field == 24)
    }

    pub(crate) fn consume(self) -> anyhow::Result<VersionsEntry<'a>> {
        if self.next_field == 24 {
            Ok(self.inner)
        } else {
            bail!(
                "not enough fields entered in version entry builder, required 24, got {}",
                self.next_field
            );
        }
    }
}

fn parse_yanked_bool(value: &str) -> anyhow::Result<bool> {
    if value == "f" {
        Ok(false)
    } else if value == "t" {
        Ok(true)
    } else {
        bail!("unexpected 'yanked' value of '{value}'")
    }
}
