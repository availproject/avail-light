// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Persistent storage for a [`chain_information::ChainInformation`] based on the browser's local
//! storage.
//!
//! The browser's local storage is a storage space in the form string that browser offer to
//! web pages. While it is very limited in size, this size is more than enough to store a
//! serialized version of a [`chain_information::ChainInformation`].
//!
//! # Usage
//!
//! Call [`LocalStorage::open`] to access the local storage provided by the environment. An error
//! is returned if the environment doesn't provide any local storage.
//!
//! Use [`LocalStorage::chain_information`] and [`LocalStorage::set_chain_information`] to
//! respectively load and store a [`chain_information::ChainInformation`] from/to the local
//! storage.
//!
//! > **Note**: The format of the stored information (in other words, the string actually stored
//! >           in the local storage) isn't documented here. At the time of the writing of this
//! >           comment, this format isn't stable and can break without warning. In the future,
//! >           though, a certain committment to a specific format should be done.
//!
//! # See also
//!
//! - [MDN documentation](https://developer.mozilla.org/en-US/docs/Web/API/Window/localStorage).
//!

#![cfg(feature = "wasm-bindings")]
#![cfg_attr(docsrs, doc(cfg(feature = "wasm-bindings")))]

use crate::chain::chain_information;

use core::{convert::TryFrom, fmt};
use wasm_bindgen::prelude::*;
use web_sys::Storage;

mod defs;

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
    ///
    /// Errors are expected to be extremely rare, but might happen for example if the serialized
    /// data exceeds the browser-specific limit.
    pub fn set_chain_information(
        &self,
        information: chain_information::ChainInformationRef<'_>,
    ) -> Result<(), StorageAccessError> {
        let decoded = defs::SerializedChainInformation::V1(information.into());
        let encoded = serde_json::to_string(&decoded).unwrap();
        self.inner
            .set_item("chain_information", &encoded)
            .map_err(StorageAccessError)?;
        Ok(())
    }

    /// Loads information about the chain from the local storage.
    ///
    /// This function can reasonably return an [`AccessError::Corrupted`] error if the user
    /// messed with the stored data.
    pub fn chain_information(
        &self,
    ) -> Result<Option<chain_information::ChainInformation>, AccessError> {
        let encoded = match self
            .inner
            .get_item("chain_information")
            .map_err(StorageAccessError)
            .map_err(AccessError::StorageAccess)?
        {
            Some(v) => v,
            None => return Ok(None),
        };

        let decoded: defs::SerializedChainInformation = serde_json::from_str(&encoded)
            .map_err(|e| CorruptedError(CorruptedErrorInner::Serde(e)))
            .map_err(AccessError::Corrupted)?;

        match decoded {
            defs::SerializedChainInformation::V1(decoded) => {
                Ok(Some(TryFrom::try_from(decoded).map_err(|err| {
                    AccessError::Corrupted(CorruptedError(CorruptedErrorInner::Deserialize(err)))
                })?))
            }
        }
    }
}

impl fmt::Debug for LocalStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalStorage")
            .field(
                "state",
                &match self.inner.get_item("chain_information") {
                    Ok(Some(_)) => "entry-present",
                    Ok(None) => "entry-absent",
                    Err(_) => "access-error",
                },
            )
            .finish()
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
    StorageAccess(StorageAccessError),
    /// Corruption in the data stored in the local storage.
    Corrupted(CorruptedError),
}

/// Opaque error indicating a browser-returned error in accessing the storage.
#[derive(Debug, derive_more::Display)]
#[display(fmt = "{:?}", _0)]
pub struct StorageAccessError(JsValue);

impl From<StorageAccessError> for JsValue {
    fn from(err: StorageAccessError) -> JsValue {
        err.0
    }
}

/// Opaque error indicating a corruption in the data stored in the local storage.
#[derive(Debug, derive_more::Display)]
#[display(fmt = "{}", _0)]
pub struct CorruptedError(CorruptedErrorInner);

#[derive(Debug, derive_more::Display)]
enum CorruptedErrorInner {
    #[display(fmt = "{}", _0)]
    Serde(serde_json::Error),
    #[display(fmt = "{}", _0)]
    Deserialize(defs::DeserializeError),
}
