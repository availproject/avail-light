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
    /// Tries to open the database from the browser environment.
    pub async fn open(db_name: &str) -> Result<Self, OpenError> {
        // TODO: also allow `WorkerGlobalScope`
        let window = web_sys::window().ok_or(OpenError::NoWindow)?;
        let idb_factory = window
            .indexed_db()
            .map_err(OpenError::IndexedDbNotSupported)?
            .unwrap();
        let open_request = idb_factory.open_with_u32(db_name, 1).unwrap();

        // Used to signal when the open request is complete.
        let (tx, rx) = oneshot::channel();

        let on_finish = Closure::once_into_js(move |_: &Event| {
            let _ = tx.send(());
        });
        open_request.set_onsuccess(Some(&on_finish.dyn_ref().unwrap()));
        open_request.set_onerror(Some(&on_finish.dyn_ref().unwrap()));

        let on_upgrade_needed = Closure::once(move |event: &Event| {
            let database = event
                .target()
                .unwrap()
                .dyn_into::<web_sys::IdbRequest>()
                .unwrap()
                .result()
                .unwrap()
                .dyn_into::<IdbDatabase>()
                .unwrap();

            // TODO: do the multiple versions handling properly?
            // Keys are block hashes, and values are SCALE-encoded block headers.
            database.create_object_store("block_headers").unwrap();
        });
        open_request.set_onupgradeneeded(Some(&on_upgrade_needed.as_ref().dyn_ref().unwrap()));

        // Block until either `onsuccess` or `onerror` happens.
        let _ = rx.await.unwrap();

        // `result()` would return an error if the request wasn't complete yet.
        let result = open_request.result().unwrap();
        match result.dyn_into::<IdbDatabase>() {
            Ok(db) => Ok(Database {
                inner: send_wrapper::SendWrapper::new(db),
            }),
            Err(err) => Err(OpenError::OpenError(err)),
        }
    }

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

/// Error when opening the database.
#[derive(Debug, derive_more::Display)]
pub enum OpenError {
    NoWindow,
    /// IndexedDB is not supported by the environment.
    #[display(fmt = "IndexedDB is not supported by the environment: {:?}", _0)]
    IndexedDbNotSupported(JsValue),
    /// The `IDBOpenDBRequest` produced an error.
    #[display(fmt = "The `IDBOpenDBRequest` produced an error: {:?}", _0)]
    OpenError(JsValue),
}

#[derive(Debug, derive_more::Display)]
pub enum AccessError {
    Corrupted(CorruptedError),
}

#[derive(Debug, derive_more::Display)]
pub enum CorruptedError {
    UnexpectedValueTy,
}
