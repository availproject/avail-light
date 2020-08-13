//! Persistent storage for light-client data based on IndexedDB, the in-brower database.

// TODO: obviously very work-in-progress

#![cfg(feature = "wasm-bindings")]
#![cfg_attr(docsrs, doc(cfg(feature = "wasm-bindings")))]

use futures::channel::oneshot;
use js_sys::{Array, ArrayBuffer, Uint8Array};
use wasm_bindgen::{prelude::*, JsCast as _};
use web_sys::{DomException, Event, IdbDatabase, IdbTransactionMode};

/// An open database.
pub struct Database {
    inner: send_wrapper::SendWrapper<IdbDatabase>,
}

impl Database {
    /// Reads one value at the given key.
    ///
    /// # Panic
    ///
    /// Panics if the `column_name` is invalid.
    ///
    async fn get(&self, column_name: &str, key: &str) -> Result<Option<String>, AccessError> {
        let transaction = self
            .inner
            .transaction_with_str_and_mode(column_name, IdbTransactionMode::Readonly)
            .unwrap();

        let store = transaction.object_store(column_name).unwrap();
        let query = match store.get(&JsValue::from_str(key)) {
            Ok(r) => r,
            Err(err) => {
                let err = err.dyn_into::<DomException>().unwrap();
                if err.name() == "DataError" {
                    return Ok(None);
                }
                panic!("Unexpected database error: {:?}")
            }
        };

        let (tx, rx) = oneshot::channel();

        // `once_into_js` de-allocates the closure only after it has been called. It is an
        // error to call it multiple times, and if it is not called, it will leak.
        // For this reason, we use the same callback on both success and failure.
        let on_finish = Closure::once_into_js(move |_: &Event| {
            let _ = tx.send(());
        });

        query.set_onsuccess(Some(&on_finish.dyn_ref().unwrap()));
        query.set_onerror(Some(&on_finish.dyn_ref().unwrap()));

        // Block until either `onsuccess` or `onerror` happens.
        let _ = rx.await.unwrap();

        if let Some(result) = query.result().unwrap().as_string() {
            Ok(Some(result))
        } else {
            Err(AccessError::Corrupted(CorruptedError::UnexpectedValueTy))
        }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        self.inner.close();
    }
}

#[derive(Debug, derive_more::Display)]
pub enum AccessError {
    Corrupted(CorruptedError),
}

#[derive(Debug, derive_more::Display)]
pub enum CorruptedError {
    UnexpectedValueTy,
}
