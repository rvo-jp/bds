//! Typed records under the root-owned state directory.
//! A record owns its filename, permissions and durable replacement/removal policy.
use crate::{config::Config, util};
use anyhow::Result;
use serde::{Serialize, de::DeserializeOwned};
use std::{fs, marker::PhantomData, path::PathBuf};

pub(crate) struct Record<T> {
    filename: &'static str,
    mode: u32,
    value: PhantomData<T>,
}
impl<T> Record<T> {
    pub(crate) const fn new(filename: &'static str, mode: u32) -> Self {
        Self {
            filename,
            mode,
            value: PhantomData,
        }
    }
    pub(crate) fn path(&self, c: &Config) -> PathBuf {
        c.state().join(self.filename)
    }
    pub(crate) fn exists(&self, c: &Config) -> bool {
        self.path(c).exists()
    }
    pub(crate) fn remove(&self, c: &Config) -> Result<()> {
        match fs::remove_file(self.path(c)) {
            Ok(()) => fs::File::open(c.state())?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
}
impl<T: DeserializeOwned> Record<T> {
    pub(crate) fn load(&self, c: &Config) -> Result<T> {
        util::read_json(&self.path(c))
    }
}
impl<T: Serialize> Record<T> {
    pub(crate) fn save(&self, c: &Config, value: &T) -> Result<()> {
        util::json(&self.path(c), value, self.mode)
    }
}

pub fn lock(c: &Config, wait_seconds: u64) -> Result<util::Lock> {
    util::Lock::acquire(&c.paths.store.join("maintenance.lock"), wait_seconds)
}
