use super::kad_mem_providers::{Providers, ProvidersConfig};
use crate::data::KADEMLIA_STORE_CF;
use codec::{Decode, Encode};
use libp2p::identity::PeerId;
use libp2p::kad::store::{Error, RecordStore, Result};
use libp2p::kad::{self, KBucketKey, ProviderRecord, Record, RecordKey};
use rocksdb::{BoundColumnFamily, IteratorMode};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::hash_set;
use std::iter;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, instrument, Level};
use {rocksdb::WriteBatch, tracing::info};

#[derive(Serialize, Deserialize, Encode, Decode, Clone)]
pub struct Entry(pub Vec<u8>, pub KadRecord);

#[derive(Serialize, Deserialize, Encode, Decode, Clone)]
pub struct KadRecord {
	value: Vec<u8>,
	publisher: Vec<u8>,
	ttl: u32,
}

// 1 is minimum value if `expires` is set because 0 means "does not expire"
fn ttl(expires: Instant) -> u32 {
	(expires - Instant::now())
		.max(Duration::from_secs(1))
		.as_secs() as u32
}

impl From<kad::Record> for Entry {
	fn from(record: kad::Record) -> Self {
		Entry(
			record.key.to_vec(),
			KadRecord {
				value: record.value,
				publisher: record.publisher.map(PeerId::to_bytes).unwrap_or_default(),
				ttl: record.expires.map(ttl).unwrap_or(0),
			},
		)
	}
}

impl From<Entry> for kad::Record {
	fn from(entry: Entry) -> Self {
		let Entry(key, record) = entry;

		kad::Record {
			key: RecordKey::from(key),
			value: record.value,
			publisher: (!record.publisher.is_empty())
				.then(|| PeerId::from_bytes(&record.publisher).expect("Invalid peer ID")),
			expires: (record.ttl > 0)
				.then(|| Instant::now() + Duration::from_secs(record.ttl.into())),
		}
	}
}

/// RocksDB implementation of a `RecordStore`.
/// Providers are kept in memory.
pub struct RocksDBStore {
	/// The identity of the peer owning the store.
	local_key: KBucketKey<PeerId>,
	/// The configuration of the store.
	config: RocksDBStoreConfig,
	/// The stored (regular) records.
	records: Arc<rocksdb::DB>,
	/// The stored provider records.
	providers: Providers,
}

/// Configuration for a `RocksDBStore`.
/// There is no limit for maximum number of records like in MemoryStore.
#[derive(Debug, Clone)]
pub struct RocksDBStoreConfig {
	/// The maximum size of record values, in bytes.
	pub max_value_bytes: usize,
	pub providers: ProvidersConfig,
}

impl Default for RocksDBStoreConfig {
	// Default values kept in line with libp2p
	fn default() -> Self {
		Self {
			max_value_bytes: 65 * 1024,
			providers: Default::default(),
		}
	}
}

impl RocksDBStore {
	/// Creates a new `RocksDBRecordStore` with the given configuration.
	pub fn with_config(local_id: PeerId, config: RocksDBStoreConfig, db: Arc<rocksdb::DB>) -> Self {
		RocksDBStore {
			local_key: KBucketKey::from(local_id),
			records: db,
			providers: Providers::with_config(config.providers.clone()),
			config,
		}
	}

	#[instrument(level = Level::TRACE, skip(self, f))]
	/// Retains records that satisfy a given predicate.
	/// NOTE: This is a suboptimal implementation for store size optimization,
	/// since its execution time scales linearly with the store size.
	/// Expiry is handled lazily when retrieving a record, and to manage expired records
	/// that are not requested, we can utilize the RocksDB TTL feature.
	/// This feature is configured per column family and must be
	/// greater than or equal to the maximum possible expiry configuration,
	/// but this should be sufficient to keep the size of the database on disk capped.
	pub fn retain<F>(&mut self, mut f: F)
	where
		F: FnMut(&RecordKey, &Record) -> bool,
	{
		let mut write_batch = WriteBatch::default();

		self.records()
			.filter(|record| !f(&record.key, record))
			.for_each(|record| write_batch.delete(record.key.clone()));

		let write_batch_len = write_batch.len();
		match self.records.write(write_batch) {
			Err(error) => error!("Failed to retain records that satisfies the predicate: {error}"),
			Ok(_) => info!("Removed {write_batch_len} records from the RocksDB store"),
		}
	}

	// Optimizations are not implemented currently
	pub fn shrink_hashmap(&mut self) {}
}

impl RocksDBStore {
	#[instrument(level = Level::TRACE, skip(self))]
	pub fn get_cf(&self) -> Option<Arc<BoundColumnFamily>> {
		let Some(cf) = self.records.cf_handle(KADEMLIA_STORE_CF) else {
			error!("Couldn't get column family \"{KADEMLIA_STORE_CF}\" handle");
			return None;
		};
		Some(cf)
	}
}

pub fn into_kad_record(record: (Vec<u8>, Vec<u8>)) -> kad::Record {
	let (key, value) = record;
	KadRecord::decode(&mut &value[..])
		.map(|record| Entry(key, record).into())
		.expect("Expected valid encoded record, got invalid")
}

// NOTE: We are using `Error::ValueTooLarge` as default error for the RocksDB store
// errors, since there is no dedicated enum variant in the RecordStore interface
use Error::ValueTooLarge as RocksDBStoreError;

impl RecordStore for RocksDBStore {
	type RecordsIter<'a> = Box<dyn Iterator<Item = Cow<'a, kad::Record>> + 'a>;

	type ProvidedIter<'a> = iter::Map<
		hash_set::Iter<'a, ProviderRecord>,
		fn(&'a ProviderRecord) -> Cow<'a, ProviderRecord>,
	>;

	#[instrument(level = Level::TRACE, skip(self))]
	fn get(&self, key: &RecordKey) -> Option<Cow<'_, Record>> {
		match self.records.get_cf(&self.get_cf()?, key) {
			Ok(record) => record
				.map(|value| (key.to_vec(), value))
				.map(into_kad_record)
				.map(Cow::Owned),
			Err(error) => {
				error!("Failed to get record from database: {error}");
				None
			},
		}
	}

	#[instrument(level = Level::TRACE, skip(self))]
	fn put(&mut self, r: Record) -> Result<()> {
		let now = Instant::now();
		let cf = self.get_cf().ok_or(RocksDBStoreError)?;

		if r.value.len() >= self.config.max_value_bytes {
			return Err(RocksDBStoreError);
		}

		let Entry(key, record) = r.into();

		let result = self
			.records
			.put_cf(&cf, key, record.encode())
			.map_err(|error| {
				error!("Failed to put record into database: {error}");
				RocksDBStoreError
			});

		if record.value.len() > 15 && record.value[15] % 100 == 1 {
			info!("Kademlia put length: {}ms", now.elapsed().as_millis());
		}

		result
	}

	#[instrument(level = Level::TRACE, skip(self))]
	fn remove(&mut self, k: &RecordKey) {
		let Some(cf) = self.get_cf() else {
			return;
		};
		let Err(error) = self.records.delete_cf(&cf, k) else {
			return;
		};
		error!("Failed to delete record from database: {error}");
	}

	#[instrument(level = "trace", skip(self))]
	fn records(&self) -> Self::RecordsIter<'_> {
		let Some(cf) = self.get_cf() else {
			return Box::new(iter::empty::<kad::Record>().map(Cow::Owned));
		};

		Box::new(
			self.records
				.full_iterator_cf(&cf, IteratorMode::Start)
				.filter_map(|result| {
					if let Err(error) = &result {
						error!("Failed to read record from database: {error}");
					}
					result.ok()
				})
				.map(|(key, value)| (key.to_vec(), value.to_vec()))
				.map(into_kad_record)
				.map(Cow::Owned),
		)
	}

	fn add_provider(&mut self, record: ProviderRecord) -> Result<()> {
		self.providers.add_provider(self.local_key, record)
	}

	fn providers(&self, key: &RecordKey) -> Vec<ProviderRecord> {
		self.providers.providers(key)
	}

	fn provided(&self) -> Self::ProvidedIter<'_> {
		self.providers.provided()
	}

	fn remove_provider(&mut self, key: &RecordKey, provider: &PeerId) {
		self.providers.remove_provider(key, provider)
	}
}

pub use ttl::ExpirationCompactionFilterFactory;

mod ttl {
	use super::into_kad_record;
	use rocksdb::{
		compaction_filter::CompactionFilter,
		compaction_filter_factory::{CompactionFilterContext, CompactionFilterFactory},
		CompactionDecision,
	};
	use std::{ffi::CString, time::Instant};

	pub struct ExpirationCompactionFilter {
		now: Instant,
		name: CString,
	}

	impl CompactionFilter for ExpirationCompactionFilter {
		fn filter(&mut self, _level: u32, key: &[u8], value: &[u8]) -> CompactionDecision {
			let record = into_kad_record((key.to_vec(), value.to_vec()));
			match record.is_expired(self.now) {
				true => CompactionDecision::Remove,
				false => CompactionDecision::Keep,
			}
		}

		fn name(&self) -> &std::ffi::CStr {
			&self.name
		}
	}

	pub struct ExpirationCompactionFilterFactory {
		name: CString,
	}

	impl Default for ExpirationCompactionFilterFactory {
		fn default() -> Self {
			let name = CString::new("kademlia_store_expiration_compaction_filter_factory")
				.expect("CString::new failed");

			ExpirationCompactionFilterFactory { name }
		}
	}

	impl CompactionFilterFactory for ExpirationCompactionFilterFactory {
		type Filter = ExpirationCompactionFilter;

		fn create(&mut self, _context: CompactionFilterContext) -> Self::Filter {
			let name =
				CString::new("kademlia_store_expiration_compaction_filter").expect("valid CString");
			ExpirationCompactionFilter {
				now: Instant::now(),
				name,
			}
		}

		fn name(&self) -> &std::ffi::CStr {
			&self.name
		}
	}
}
