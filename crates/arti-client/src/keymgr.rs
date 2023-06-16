//! A dummy key manager implementation.
//!
//! If arti-client is compiled with the `keymgr` feature, [`TorClient`](crate::TorClient) will use
//! the key manager implementation from the `tor-keymgr` crate.
//!
//! If arti-client is compiled without the `keymgr` feature, then [`TorClient`](crate::TorClient)
//! will use this no-op key manager implementation instead.

use std::any::Any;
use tor_error::{ErrorKind, HasKind};

/// A dummy key manager implementation.
///
/// This implementation has the same API as the key manager from the `tor-keymgr` crate, except all
/// its operations are no-ops.
//
// TODO hs: Make KeyMgr have the same constructor API as tor-keymgr::KeyMgr #897
// TODO hs: Remove the generic param and inner error from KeyMgr and expose a KeyMgrError instead.
#[derive(Copy, Clone, Debug)]
pub(crate) struct KeyMgr;

/// An error type for the dummy key manager.
#[derive(Copy, Clone, Debug, thiserror::Error)]
#[error("Cannot access key store: `keymgr` feature is disabled")]
pub struct Error;

impl HasKind for Error {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// A result type for this module.
pub(crate) type Result<T> = std::result::Result<T, Error>;

impl KeyMgr {
    /// A dummy `get` implementation that always behaves like the requested key is not found.
    ///
    /// This function always returns `Ok(None)`.
    //
    // TODO hs: remove
    #[allow(unused)]
    // We need to allow these lints because this impl needs to mirror that of `tor_keymgr::KeyMgr`.
    #[allow(clippy::unnecessary_wraps, clippy::extra_unused_type_parameters)]
    pub(crate) fn get<K>(&self, _: &dyn Any) -> Result<Option<K>> {
        Ok(None)
    }

    /// A dummy `insert` implementation that always behaves as if the `insert` operation was
    /// successful.
    ///
    /// This function always returns `Ok(())`.
    //
    // TODO hs: remove
    #[allow(unused)]
    // We need to allow these lints because this impl needs to mirror that of `tor_keymgr::KeyMgr`.
    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn insert<K>(&self, _: K, _: &dyn Any) -> Result<()> {
        Ok(())
    }

    /// A dummy `remove` implementation that always behaves as if the `remove` operation was
    /// successful.
    ///
    /// This function always returns `Ok(())`.
    //
    // TODO hs: remove
    #[allow(unused)]
    // We need to allow these lints because this impl needs to mirror that of `tor_keymgr::KeyMgr`.
    #[allow(clippy::unnecessary_wraps, clippy::extra_unused_type_parameters)]
    pub(crate) fn remove<K>(&self, _: &dyn Any) -> Result<()> {
        Ok(())
    }
}
