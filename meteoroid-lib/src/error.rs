use std::fmt::{Display, Formatter};

pub struct ErrFmt<'a>(&'a (dyn std::error::Error + Send + Sync));

#[inline]
pub fn unpack(e: &(dyn std::error::Error + Send + Sync)) -> ErrFmt<'_> {
    ErrFmt(e)
}

impl Display for ErrFmt<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}", self.0))?;
        let mut src = self.0.source();
        while let Some(e) = src {
            f.write_fmt(format_args!(" -> {e}"))?;
            src = e.source();
        }
        Ok(())
    }
}
