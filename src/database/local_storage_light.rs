//! Persistent storage for light-client data based on the browser's local storage.

#![cfg(feature = "wasm-bindings")]
#![cfg_attr(docsrs, doc(cfg(feature = "wasm-bindings")))]

use crate::{chain::chain_information, header};

mod defs;

use core::fmt;
use wasm_bindgen::prelude::*;
use web_sys::Storage;

/// An open local storage. Corresponds to
/// [a JavaScript `Storage` object](https://developer.mozilla.org/en-US/docs/Web/API/Storage).
pub struct LocalStorage {
    inner: send_wrapper::SendWrapper<Storage>,
}

impl LocalStorage {
    /// Tries to open the storage from the browser environment.
    pub async fn open() -> Result<Self, OpenError> {
        let window = web_sys::window().ok_or(OpenError::NoWindow)?;
        let storage = window
            .local_storage()
            .map_err(OpenError::LocalStorageNotSupported)?
            .unwrap();

        Ok(LocalStorage {
            inner: send_wrapper::SendWrapper::new(storage),
        })
    }

    /// Stores the given information in the local storage.
    pub fn set_chain_information(
        &self,
        information: chain_information::ChainInformationRef<'_>,
    ) -> Result<(), AccessError> {
        let decoded = defs::SerializedChainInformation::V1(information.into());
        let encoded = serde_json::to_string(&decoded).unwrap();
        self.inner
            .set_item("chain_information", &encoded)
            .map_err(AccessError::StorageAccess)?;
        Ok(())
    }

    /// Loads information about the chain from the local storage.
    pub fn chain_information(
        &self,
    ) -> Result<Option<chain_information::ChainInformation>, AccessError> {
        let encoded = match self
            .inner
            .get_item("chain_information")
            .map_err(AccessError::StorageAccess)?
        {
            Some(v) => v,
            None => return Ok(None),
        };

        let decoded: defs::SerializedChainInformation = serde_json::from_str(&encoded)
            .map_err(CorruptedError)
            .map_err(AccessError::Corrupted)?;

        match decoded {
            defs::SerializedChainInformation::V1(decoded) => Ok(Some(decoded.into())),
        }
    }
}

/// Error when opening the database.
#[derive(Debug, derive_more::Display)]
pub enum OpenError {
    /// No `window` object available.
    ///
    /// > **Note**: This probably indicates that the environment is not a browser.
    NoWindow,
    /// Local storage is not supported by the environment.
    #[display(fmt = "Local storage is not supported by the environment: {:?}", _0)]
    LocalStorageNotSupported(JsValue),
}

/// Error accessing the database.
#[derive(Debug, derive_more::Display)]
pub enum AccessError {
    /// JavaScript error produced when accessing the storage.
    #[display(fmt = "Error when accessing local storage: {:?}", _0)]
    StorageAccess(JsValue),
    /// Corruption in the data stored in the local storage.
    Corrupted(CorruptedError),
}

/// Opaque error indicating a corruption in the data stored in the local storage.
#[derive(Debug, derive_more::Display)]
pub struct CorruptedError(serde_json::Error);
