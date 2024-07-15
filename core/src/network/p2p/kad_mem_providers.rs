use libp2p::identity::PeerId;
use libp2p::kad::store::{Error, Result};
use libp2p::kad::{KBucketKey, ProviderRecord, RecordKey, K_VALUE};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::{hash_map, hash_set, HashMap, HashSet};
use std::iter;

#[derive(Clone, Debug)]
pub struct ProvidersConfig {
	/// The maximum number of providers stored for a key.
	///
	/// This should match up with the chosen replication factor.
	pub max_providers_per_key: usize,
	/// The maximum number of provider records for which the
	/// local node is the provider.
	pub max_provided_keys: usize,
}

impl Default for ProvidersConfig {
	// Default values kept in line with libp2p
	fn default() -> Self {
		Self {
			max_provided_keys: 1024,
			max_providers_per_key: K_VALUE.get(),
		}
	}
}

pub struct Providers {
	/// Providers configuration
	config: ProvidersConfig,
	/// The stored provider records.
	providers: HashMap<RecordKey, SmallVec<[ProviderRecord; K_VALUE.get()]>>,
	/// The set of all provider records for the node identified by `local_key`.
	/// Must be kept in sync with `providers`.
	provided: HashSet<ProviderRecord>,
}

pub type ProviderIter<'a> = iter::Map<
	hash_set::Iter<'a, ProviderRecord>,
	fn(&'a ProviderRecord) -> Cow<'a, ProviderRecord>,
>;

impl Providers {
	pub fn with_config(config: ProvidersConfig) -> Self {
		Providers {
			config,
			providers: Default::default(),
			provided: Default::default(),
		}
	}

	pub fn add_provider(
		&mut self,
		local_key: KBucketKey<PeerId>,
		record: ProviderRecord,
	) -> Result<()> {
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

	pub fn providers(&self, key: &RecordKey) -> Vec<ProviderRecord> {
		self.providers
			.get(key)
			.map_or_else(Vec::new, |ps| ps.clone().into_vec())
	}

	pub fn provided(&self) -> ProviderIter<'_> {
		self.provided.iter().map(Cow::Borrowed)
	}

	pub fn remove_provider(&mut self, key: &RecordKey, provider: &PeerId) {
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
