// Copyright 2019 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

use libp2p::identity::PeerId;
use libp2p::kad::store::{Error, RecordStore, Result};
use libp2p::kad::{KBucketKey, ProviderRecord, Record, RecordKey, K_VALUE};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::{hash_map, hash_set, HashMap, HashSet};
use std::iter;
use tracing::trace;

/// In-memory implementation of a `RecordStore`.
pub struct MemoryStore {
	/// The identity of the peer owning the store.
	local_key: KBucketKey<PeerId>,
	/// The configuration of the store.
	config: MemoryStoreConfig,
	/// The stored (regular) records.
	records: HashMap<RecordKey, Record>,
	/// The stored provider records.
	providers: HashMap<RecordKey, SmallVec<[ProviderRecord; K_VALUE.get()]>>,
	/// The set of all provider records for the node identified by `local_key`.
	///
	/// Must be kept in sync with `providers`.
	provided: HashSet<ProviderRecord>,
}

/// Configuration for a `MemoryStore`.
#[derive(Debug, Clone)]
pub struct MemoryStoreConfig {
	/// The maximum number of records.
	pub max_records: usize,
	/// The maximum size of record values, in bytes.
	pub max_value_bytes: usize,
	/// The maximum number of providers stored for a key.
	///
	/// This should match up with the chosen replication factor.
	pub max_providers_per_key: usize,
	/// The maximum number of provider records for which the
	/// local node is the provider.
	pub max_provided_keys: usize,
}

impl Default for MemoryStoreConfig {
	// Default values kept in line with libp2p
	fn default() -> Self {
		Self {
			max_records: 1024,
			max_value_bytes: 65 * 1024,
			max_provided_keys: 1024,
			max_providers_per_key: K_VALUE.get(),
		}
	}
}

impl MemoryStore {
	#[allow(dead_code)]
	/// Creates a new `MemoryRecordStore` with a default configuration.
	pub fn new(local_id: PeerId) -> Self {
		Self::with_config(local_id, Default::default())
	}

	/// Creates a new `MemoryRecordStore` with the given configuration.
	pub fn with_config(local_id: PeerId, config: MemoryStoreConfig) -> Self {
		MemoryStore {
			local_key: KBucketKey::from(local_id),
			config,
			records: HashMap::default(),
			provided: HashSet::default(),
			providers: HashMap::default(),
		}
	}

	#[allow(dead_code)]
	/// Retains the records satisfying a predicate.
	pub fn retain<F>(&mut self, f: F)
	where
		F: FnMut(&RecordKey, &mut Record) -> bool,
	{
		self.records.retain(f);
	}

	// Inserts a record into the record store
	pub fn put(&mut self, r: Record) -> Result<()> {
		RecordStore::put(self, r)
	}

	// Return an iterator over records
	pub fn records_iter(&mut self) -> std::collections::hash_map::Iter<'_, RecordKey, Record> {
		self.records.iter()
	}

	/// Shrinks the capacity of hashmap as much as possible
	pub fn shrink_hashmap(&mut self) {
		self.records.shrink_to_fit();

		trace!(
			"Memory store - Len: {:?}. Capacity: {:?}",
			self.records.len(),
			self.records.capacity()
		);
	}
}

impl RecordStore for MemoryStore {
	type RecordsIter<'a> =
		iter::Map<hash_map::Values<'a, RecordKey, Record>, fn(&'a Record) -> Cow<'a, Record>>;

	type ProvidedIter<'a> = iter::Map<
		hash_set::Iter<'a, ProviderRecord>,
		fn(&'a ProviderRecord) -> Cow<'a, ProviderRecord>,
	>;

	fn get(&self, k: &RecordKey) -> Option<Cow<'_, Record>> {
		self.records.get(k).map(Cow::Borrowed)
	}

	fn put(&mut self, r: Record) -> Result<()> {
		if r.value.len() >= self.config.max_value_bytes {
			return Err(Error::ValueTooLarge);
		}

		let num_records = self.records.len();

		match self.records.entry(r.key.clone()) {
			hash_map::Entry::Occupied(mut e) => {
				e.insert(r);
			},
			hash_map::Entry::Vacant(e) => {
				if num_records >= self.config.max_records {
					return Err(Error::MaxRecords);
				}
				e.insert(r);
			},
		}

		Ok(())
	}

	fn remove(&mut self, k: &RecordKey) {
		self.records.remove(k);
	}

	fn records(&self) -> Self::RecordsIter<'_> {
		self.records.values().map(Cow::Borrowed)
	}

	fn add_provider(&mut self, record: ProviderRecord) -> Result<()> {
		let num_keys = self.providers.len();

		// Obtain the entry
		let providers = match self.providers.entry(record.key.clone()) {
			e @ hash_map::Entry::Occupied(_) => e,
			e @ hash_map::Entry::Vacant(_) => {
				if self.config.max_provided_keys == num_keys {
					return Err(Error::MaxProvidedKeys);
				}
				e
			},
		}
		.or_insert_with(Default::default);

		if let Some(i) = providers.iter().position(|p| p.provider == record.provider) {
			// In-place update of an existing provider record.
			providers.as_mut()[i] = record;
		} else {
			// It is a new provider record for that key.
			let local_key = self.local_key.clone();
			let key = KBucketKey::new(record.key.clone());
			let provider = KBucketKey::from(record.provider);
			if let Some(i) = providers.iter().position(|p| {
				let pk = KBucketKey::from(p.provider);
				provider.distance(&key) < pk.distance(&key)
			}) {
				// Insert the new provider.
				if local_key.preimage() == &record.provider {
					self.provided.insert(record.clone());
				}
				providers.insert(i, record);
				// Remove the excess provider, if any.
				if providers.len() > self.config.max_providers_per_key {
					if let Some(p) = providers.pop() {
						self.provided.remove(&p);
					}
				}
			} else if providers.len() < self.config.max_providers_per_key {
				// The distance of the new provider to the key is larger than
				// the distance of any existing provider, but there is still room.
				if local_key.preimage() == &record.provider {
					self.provided.insert(record.clone());
				}
				providers.push(record);
			}
		}
		Ok(())
	}

	fn providers(&self, key: &RecordKey) -> Vec<ProviderRecord> {
		self.providers
			.get(key)
			.map_or_else(Vec::new, |ps| ps.clone().into_vec())
	}

	fn provided(&self) -> Self::ProvidedIter<'_> {
		self.provided.iter().map(Cow::Borrowed)
	}

	fn remove_provider(&mut self, key: &RecordKey, provider: &PeerId) {
		if let hash_map::Entry::Occupied(mut e) = self.providers.entry(key.clone()) {
			let providers = e.get_mut();
			if let Some(i) = providers.iter().position(|p| &p.provider == provider) {
				let p = providers.remove(i);
				self.provided.remove(&p);
			}
			if providers.is_empty() {
				e.remove();
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use super::*;
	use libp2p::{
		kad::{KBucketDistance, RecordKey},
		multihash::Multihash,
	};
	use proptest::{
		prelude::{any, any_with},
		proptest,
		sample::size_range,
		strategy::Strategy,
	};
	use rand::Rng;

	const SHA_256_MH: u64 = 0x12;
	const MULTIHASH_SIZE: usize = 32;

	fn random_multihash() -> Multihash<MULTIHASH_SIZE> {
		Multihash::wrap(SHA_256_MH, &rand::thread_rng().gen::<[u8; 32]>()).unwrap()
	}

	fn distance(r: &ProviderRecord) -> KBucketDistance {
		KBucketKey::new(r.key.clone()).distance(&KBucketKey::from(r.provider))
	}

	fn arb_key() -> impl Strategy<Value = RecordKey> {
		any::<[u8; 32]>().prop_map(|hash| {
			RecordKey::from(Multihash::<MULTIHASH_SIZE>::wrap(SHA_256_MH, &hash).unwrap())
		})
	}

	fn arb_publisher() -> impl Strategy<Value = Option<PeerId>> {
		any::<bool>().prop_map(|has_publisher| has_publisher.then(PeerId::random))
	}

	fn arb_expires() -> impl Strategy<Value = Option<Instant>> {
		(any::<bool>(), 0..60u64).prop_map(|(expires, seconds)| {
			expires.then(|| Instant::now() + Duration::from_secs(seconds))
		})
	}

	fn arb_record() -> impl Strategy<Value = Record> {
		(
			arb_key(),
			any_with::<Vec<u8>>(size_range(1..2048).lift()),
			arb_publisher(),
			arb_expires(),
		)
			.prop_map(|(key, value, publisher, expires)| Record {
				key,
				value,
				publisher,
				expires,
			})
	}

	fn arb_provider_record() -> impl Strategy<Value = ProviderRecord> {
		(arb_key(), arb_expires()).prop_map(|(key, expires)| ProviderRecord {
			key,
			provider: PeerId::random(),
			expires,
			addresses: vec![],
		})
	}

	proptest! {
	#[test]
	fn put_get_remove_record(r in arb_record()) {
		let mut store = MemoryStore::new(PeerId::random());
		assert!(store.put(r.clone()).is_ok());
		assert_eq!(Some(Cow::Borrowed(&r)), store.get(&r.key));
		store.remove(&r.key);
		assert!(store.get(&r.key).is_none());
	}
	}

	proptest! {
	#[test]
	fn add_get_remove_provider(r in arb_provider_record()) {
		let mut store = MemoryStore::new(PeerId::random());
		assert!(store.add_provider(r.clone()).is_ok());
		assert!(store.providers(&r.key).contains(&r));
		store.remove_provider(&r.key, &r.provider);
		assert!(!store.providers(&r.key).contains(&r));
	}
	}

	proptest! {
	#[test]
	fn providers_ordered_by_distance_to_key(providers  in 0..256u32) {
		let providers = (0..providers).
			map(|_| KBucketKey::from(PeerId::random())).
				collect::<Vec<_>>();
		let mut store = MemoryStore::new(PeerId::random());
		let key = RecordKey::from(random_multihash());

		let mut records = providers
			.into_iter()
			.map(|p| ProviderRecord::new(key.clone(), p.into_preimage(), Vec::new()))
			.collect::<Vec<_>>();

		for r in &records {
			assert!(store.add_provider(r.clone()).is_ok());
		}

		records.sort_by_key(distance);
		records.truncate(store.config.max_providers_per_key);
		assert!(records == store.providers(&key).to_vec())
	}
	}

	#[test]
	fn provided() {
		let id = PeerId::random();
		let mut store = MemoryStore::new(id);
		let key = random_multihash();
		let rec = ProviderRecord::new(key, id, Vec::new());
		assert!(store.add_provider(rec.clone()).is_ok());
		assert_eq!(
			vec![Cow::Borrowed(&rec)],
			store.provided().collect::<Vec<_>>()
		);
		store.remove_provider(&rec.key, &id);
		assert_eq!(store.provided().count(), 0);
	}

	#[test]
	fn update_provider() {
		let mut store = MemoryStore::new(PeerId::random());
		let key = random_multihash();
		let prv = PeerId::random();
		let mut rec = ProviderRecord::new(key, prv, Vec::new());
		assert!(store.add_provider(rec.clone()).is_ok());
		assert_eq!(vec![rec.clone()], store.providers(&rec.key).to_vec());
		rec.expires = Some(Instant::now());
		assert!(store.add_provider(rec.clone()).is_ok());
		assert_eq!(vec![rec.clone()], store.providers(&rec.key).to_vec());
	}

	#[test]
	fn max_provided_keys() {
		let mut store = MemoryStore::new(PeerId::random());
		for _ in 0..store.config.max_provided_keys {
			let key = random_multihash();
			let prv = PeerId::random();
			let rec = ProviderRecord::new(key, prv, Vec::new());
			let _ = store.add_provider(rec);
		}
		let key = random_multihash();
		let prv = PeerId::random();
		let rec = ProviderRecord::new(key, prv, Vec::new());
		match store.add_provider(rec) {
			Err(Error::MaxProvidedKeys) => {},
			_ => panic!("Unexpected result"),
		}
	}
}
