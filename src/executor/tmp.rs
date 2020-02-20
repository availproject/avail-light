
/// Interface for accessing the storage from within the runtime.
#[runtime_interface]
pub trait Storage {
	/// Returns the data for `key` in the storage or `None` if the key can not be found.
	fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
		self.storage(key).map(|s| s.to_vec())
	}

	/// All Child api uses :
	/// - A `child_storage_key` to define the anchor point for the child proof
	/// (commonly the location where the child root is stored in its parent trie).
	/// - A `child_storage_types` to identify the kind of the child type and how its
	/// `child definition` parameter is encoded.
	/// - A `child_definition_parameter` which is the additional information required
	/// to use the child trie. For instance defaults child tries requires this to
	/// contain a collision free unique id.
	///
	/// This function specifically returns the data for `key` in the child storage or `None`
	/// if the key can not be found.
	fn child_get(
		&self,
		child_storage_key: &[u8],
		child_definition: &[u8],
		child_type: u32,
		key: &[u8],
	) -> Option<Vec<u8>> {
		let storage_key = child_storage_key_or_panic(child_storage_key);
		let child_info = ChildInfo::resolve_child_info(child_type, child_definition)
			.expect("Invalid child definition");
		self.child_storage(storage_key, child_info, key).map(|s| s.to_vec())
	}

	/// Get `key` from storage, placing the value into `value_out` and return the number of
	/// bytes that the entry in storage has beyond the offset or `None` if the storage entry
	/// doesn't exist at all.
	/// If `value_out` length is smaller than the returned length, only `value_out` length bytes
	/// are copied into `value_out`.
	fn read(&self, key: &[u8], value_out: &mut [u8], value_offset: u32) -> Option<u32> {
		self.storage(key).map(|value| {
			let value_offset = value_offset as usize;
			let data = &value[value_offset.min(value.len())..];
			let written = std::cmp::min(data.len(), value_out.len());
			value_out[..written].copy_from_slice(&data[..written]);
			value.len() as u32
		})
	}

	/// Get `key` from child storage, placing the value into `value_out` and return the number
	/// of bytes that the entry in storage has beyond the offset or `None` if the storage entry
	/// doesn't exist at all.
	/// If `value_out` length is smaller than the returned length, only `value_out` length bytes
	/// are copied into `value_out`.
	///
	/// See `child_get` for common child api parameters.
	fn child_read(
		&self,
		child_storage_key: &[u8],
		child_definition: &[u8],
		child_type: u32,
		key: &[u8],
		value_out: &mut [u8],
		value_offset: u32,
	) -> Option<u32> {
		let storage_key = child_storage_key_or_panic(child_storage_key);
		let child_info = ChildInfo::resolve_child_info(child_type, child_definition)
			.expect("Invalid child definition");
		self.child_storage(storage_key, child_info, key)
			.map(|value| {
				let value_offset = value_offset as usize;
				let data = &value[value_offset.min(value.len())..];
				let written = std::cmp::min(data.len(), value_out.len());
				value_out[..written].copy_from_slice(&data[..written]);
				value.len() as u32
			})
	}

	/// Set `key` to `value` in the storage.
	fn set(&mut self, key: &[u8], value: &[u8]) {
		self.set_storage(key.to_vec(), value.to_vec());
	}

	/// Set `key` to `value` in the child storage denoted by `child_storage_key`.
	///
	/// See `child_get` for common child api parameters.
	fn child_set(
		&mut self,
		child_storage_key: &[u8],
		child_definition: &[u8],
		child_type: u32,
		key: &[u8],
		value: &[u8],
	) {
		let storage_key = child_storage_key_or_panic(child_storage_key);
		let child_info = ChildInfo::resolve_child_info(child_type, child_definition)
			.expect("Invalid child definition");
		self.set_child_storage(storage_key, child_info, key.to_vec(), value.to_vec());
	}

	/// Clear the storage of the given `key` and its value.
	fn clear(&mut self, key: &[u8]) {
		self.clear_storage(key)
	}

	/// Clear the given child storage of the given `key` and its value.
	///
	/// See `child_get` for common child api parameters.
	fn child_clear(
		&mut self,
		child_storage_key: &[u8],
		child_definition: &[u8],
		child_type: u32,
		key: &[u8],
	) {
		let storage_key = child_storage_key_or_panic(child_storage_key);
		let child_info = ChildInfo::resolve_child_info(child_type, child_definition)
			.expect("Invalid child definition");
		self.clear_child_storage(storage_key, child_info, key);
	}

	/// Clear an entire child storage.
	///
	/// See `child_get` for common child api parameters.
	fn child_storage_kill(
		&mut self,
		child_storage_key: &[u8],
		child_definition: &[u8],
		child_type: u32,
	) {
		let storage_key = child_storage_key_or_panic(child_storage_key);
		let child_info = ChildInfo::resolve_child_info(child_type, child_definition)
			.expect("Invalid child definition");
		self.kill_child_storage(storage_key, child_info);
	}

	/// Check whether the given `key` exists in storage.
	fn exists(&self, key: &[u8]) -> bool {
		self.exists_storage(key)
	}

	/// Check whether the given `key` exists in storage.
	///
	/// See `child_get` for common child api parameters.
	fn child_exists(
		&self,
		child_storage_key: &[u8],
		child_definition: &[u8],
		child_type: u32,
		key: &[u8],
	) -> bool {
		let storage_key = child_storage_key_or_panic(child_storage_key);
		let child_info = ChildInfo::resolve_child_info(child_type, child_definition)
			.expect("Invalid child definition");
		self.exists_child_storage(storage_key, child_info, key)
	}

	/// Clear the storage of each key-value pair where the key starts with the given `prefix`.
	fn clear_prefix(&mut self, prefix: &[u8]) {
		Externalities::clear_prefix(*self, prefix)
	}

	/// Clear the child storage of each key-value pair where the key starts with the given `prefix`.
	///
	/// See `child_get` for common child api parameters.
	fn child_clear_prefix(
		&mut self,
		child_storage_key: &[u8],
		child_definition: &[u8],
		child_type: u32,
		prefix: &[u8],
	) {
		let storage_key = child_storage_key_or_panic(child_storage_key);
		let child_info = ChildInfo::resolve_child_info(child_type, child_definition)
			.expect("Invalid child definition");
		self.clear_child_prefix(storage_key, child_info, prefix);
	}

	/// "Commit" all existing operations and compute the resulting storage root.
	///
	/// The hashing algorithm is defined by the `Block`.
	///
	/// Returns the SCALE encoded hash.
	fn root(&mut self) -> Vec<u8> {
		self.storage_root()
	}

	/// "Commit" all existing operations and compute the resulting child storage root.
	///
	/// The hashing algorithm is defined by the `Block`.
	///
	/// Returns the SCALE encoded hash.
	///
	/// See `child_get` for common child api parameters.
	fn child_root(
		&mut self,
		child_storage_key: &[u8],
	) -> Vec<u8> {
		let storage_key = child_storage_key_or_panic(child_storage_key);
		self.child_storage_root(storage_key)
	}

	/// "Commit" all existing operations and get the resulting storage change root.
	/// `parent_hash` is a SCALE encoded hash.
	///
	/// The hashing algorithm is defined by the `Block`.
	///
	/// Returns an `Option` that holds the SCALE encoded hash.
	fn changes_root(&mut self, parent_hash: &[u8]) -> Option<Vec<u8>> {
		self.storage_changes_root(parent_hash)
			.expect("Invalid `parent_hash` given to `changes_root`.")
	}

	/// Get the next key in storage after the given one in lexicographic order.
	fn next_key(&mut self, key: &[u8]) -> Option<Vec<u8>> {
		self.next_storage_key(&key)
	}

	/// Get the next key in storage after the given one in lexicographic order in child storage.
	fn child_next_key(
		&mut self,
		child_storage_key: &[u8],
		child_definition: &[u8],
		child_type: u32,
		key: &[u8],
	) -> Option<Vec<u8>> {
		let storage_key = child_storage_key_or_panic(child_storage_key);
		let child_info = ChildInfo::resolve_child_info(child_type, child_definition)
			.expect("Invalid child definition");
		self.next_child_storage_key(storage_key, child_info, key)
	}
}

/// Interface that provides trie related functionality.
#[runtime_interface]
pub trait Trie {
	/// A trie root formed from the iterated items.
	fn blake2_256_root(input: Vec<(Vec<u8>, Vec<u8>)>) -> H256 {
		Layout::<sp_core::Blake2Hasher>::trie_root(input)
	}

	/// A trie root formed from the enumerated items.
	fn blake2_256_ordered_root(input: Vec<Vec<u8>>) -> H256 {
		Layout::<sp_core::Blake2Hasher>::ordered_trie_root(input)
	}
}

/// Interface that provides miscellaneous functions for communicating between the runtime and the node.
#[runtime_interface]
pub trait Misc {
	/// The current relay chain identifier.
	fn chain_id(&self) -> u64 {
		sp_externalities::Externalities::chain_id(*self)
	}

	/// Print a number.
	fn print_num(val: u64) {
		log::debug!(target: "runtime", "{}", val);
	}

	/// Print any valid `utf8` buffer.
	fn print_utf8(utf8: &[u8]) {
		if let Ok(data) = std::str::from_utf8(utf8) {
			log::debug!(target: "runtime", "{}", data)
		}
	}

	/// Print any `u8` slice as hex.
	fn print_hex(data: &[u8]) {
		log::debug!(target: "runtime", "{}", HexDisplay::from(&data));
	}

	/// Extract the runtime version of the given wasm blob by calling `Core_version`.
	///
	/// Returns the SCALE encoded runtime version and `None` if the call failed.
	///
	/// # Performance
	///
	/// Calling this function is very expensive and should only be done very occasionally.
	/// For getting the runtime version, it requires instantiating the wasm blob and calling a
	/// function in this blob.
	fn runtime_version(&mut self, wasm: &[u8]) -> Option<Vec<u8>> {
		// Create some dummy externalities, `Core_version` should not write data anyway.
		let mut ext = sp_state_machine::BasicExternalities::default();

		self.extension::<CallInWasmExt>()
			.expect("No `CallInWasmExt` associated for the current context!")
			.call_in_wasm(wasm, "Core_version", &[], &mut ext)
			.ok()
	}
}

/// Interfaces for working with crypto related types from within the runtime.
#[runtime_interface]
pub trait Crypto {
	/// Returns all `ed25519` public keys for the given key id from the keystore.
	fn ed25519_public_keys(&mut self, id: KeyTypeId) -> Vec<ed25519::Public> {
		self.extension::<KeystoreExt>()
			.expect("No `keystore` associated for the current context!")
			.read()
			.ed25519_public_keys(id)
	}

	/// Generate an `ed22519` key for the given key type using an optional `seed` and
	/// store it in the keystore.
	///
	/// The `seed` needs to be a valid utf8.
	///
	/// Returns the public key.
	fn ed25519_generate(&mut self, id: KeyTypeId, seed: Option<Vec<u8>>) -> ed25519::Public {
		let seed = seed.as_ref().map(|s| std::str::from_utf8(&s).expect("Seed is valid utf8!"));
		self.extension::<KeystoreExt>()
			.expect("No `keystore` associated for the current context!")
			.write()
			.ed25519_generate_new(id, seed)
			.expect("`ed25519_generate` failed")
	}

	/// Sign the given `msg` with the `ed25519` key that corresponds to the given public key and
	/// key type in the keystore.
	///
	/// Returns the signature.
	fn ed25519_sign(
		&mut self,
		id: KeyTypeId,
		pub_key: &ed25519::Public,
		msg: &[u8],
	) -> Option<ed25519::Signature> {
		self.extension::<KeystoreExt>()
			.expect("No `keystore` associated for the current context!")
			.read()
			.ed25519_key_pair(id, &pub_key)
			.map(|k| k.sign(msg))
	}

	/// Verify an `ed25519` signature.
	///
	/// Returns `true` when the verification in successful.
	fn ed25519_verify(
		&self,
		sig: &ed25519::Signature,
		msg: &[u8],
		pub_key: &ed25519::Public,
	) -> bool {
		ed25519::Pair::verify(sig, msg, pub_key)
	}

	/// Returns all `sr25519` public keys for the given key id from the keystore.
	fn sr25519_public_keys(&mut self, id: KeyTypeId) -> Vec<sr25519::Public> {
		self.extension::<KeystoreExt>()
			.expect("No `keystore` associated for the current context!")
			.read()
			.sr25519_public_keys(id)
	}

	/// Generate an `sr22519` key for the given key type using an optional seed and
	/// store it in the keystore.
	///
	/// The `seed` needs to be a valid utf8.
	///
	/// Returns the public key.
	fn sr25519_generate(&mut self, id: KeyTypeId, seed: Option<Vec<u8>>) -> sr25519::Public {
		let seed = seed.as_ref().map(|s| std::str::from_utf8(&s).expect("Seed is valid utf8!"));
		self.extension::<KeystoreExt>()
			.expect("No `keystore` associated for the current context!")
			.write()
			.sr25519_generate_new(id, seed)
			.expect("`sr25519_generate` failed")
	}

	/// Sign the given `msg` with the `sr25519` key that corresponds to the given public key and
	/// key type in the keystore.
	///
	/// Returns the signature.
	fn sr25519_sign(
		&mut self,
		id: KeyTypeId,
		pub_key: &sr25519::Public,
		msg: &[u8],
	) -> Option<sr25519::Signature> {
		self.extension::<KeystoreExt>()
			.expect("No `keystore` associated for the current context!")
			.read()
			.sr25519_key_pair(id, &pub_key)
			.map(|k| k.sign(msg))
	}

	/// Verify an `sr25519` signature.
	///
	/// Returns `true` when the verification in successful.
	fn sr25519_verify(sig: &sr25519::Signature, msg: &[u8], pubkey: &sr25519::Public) -> bool {
		sr25519::Pair::verify(sig, msg, pubkey)
	}

	/// Verify and recover a SECP256k1 ECDSA signature.
	/// - `sig` is passed in RSV format. V should be either 0/1 or 27/28.
	/// Returns `Err` if the signature is bad, otherwise the 64-byte pubkey
	/// (doesn't include the 0x04 prefix).
	fn secp256k1_ecdsa_recover(
		sig: &[u8; 65],
		msg: &[u8; 32],
	) -> Result<[u8; 64], EcdsaVerifyError> {
		let rs = secp256k1::Signature::parse_slice(&sig[0..64])
			.map_err(|_| EcdsaVerifyError::BadRS)?;
		let v = secp256k1::RecoveryId::parse(if sig[64] > 26 { sig[64] - 27 } else { sig[64] } as u8)
			.map_err(|_| EcdsaVerifyError::BadV)?;
		let pubkey = secp256k1::recover(&secp256k1::Message::parse(msg), &rs, &v)
			.map_err(|_| EcdsaVerifyError::BadSignature)?;
		let mut res = [0u8; 64];
		res.copy_from_slice(&pubkey.serialize()[1..65]);
		Ok(res)
	}

	/// Verify and recover a SECP256k1 ECDSA signature.
	/// - `sig` is passed in RSV format. V should be either 0/1 or 27/28.
	/// - returns `Err` if the signature is bad, otherwise the 33-byte compressed pubkey.
	fn secp256k1_ecdsa_recover_compressed(
		sig: &[u8; 65],
		msg: &[u8; 32],
	) -> Result<[u8; 33], EcdsaVerifyError> {
		let rs = secp256k1::Signature::parse_slice(&sig[0..64])
			.map_err(|_| EcdsaVerifyError::BadRS)?;
		let v = secp256k1::RecoveryId::parse(if sig[64] > 26 { sig[64] - 27 } else { sig[64] } as u8)
			.map_err(|_| EcdsaVerifyError::BadV)?;
		let pubkey = secp256k1::recover(&secp256k1::Message::parse(msg), &rs, &v)
			.map_err(|_| EcdsaVerifyError::BadSignature)?;
		Ok(pubkey.serialize_compressed())
	}
}

/// Interface that provides functions for hashing with different algorithms.
#[runtime_interface]
pub trait Hashing {
	/// Conduct a 256-bit Keccak hash.
	fn keccak_256(data: &[u8]) -> [u8; 32] {
		sp_core::hashing::keccak_256(data)
	}

	/// Conduct a 256-bit Sha2 hash.
	fn sha2_256(data: &[u8]) -> [u8; 32] {
		sp_core::hashing::sha2_256(data)
	}

	/// Conduct a 128-bit Blake2 hash.
	fn blake2_128(data: &[u8]) -> [u8; 16] {
		sp_core::hashing::blake2_128(data)
	}

	/// Conduct a 256-bit Blake2 hash.
	fn blake2_256(data: &[u8]) -> [u8; 32] {
		sp_core::hashing::blake2_256(data)
	}

	/// Conduct four XX hashes to give a 256-bit result.
	fn twox_256(data: &[u8]) -> [u8; 32] {
		sp_core::hashing::twox_256(data)
	}

	/// Conduct two XX hashes to give a 128-bit result.
	fn twox_128(data: &[u8]) -> [u8; 16] {
		sp_core::hashing::twox_128(data)
	}

	/// Conduct two XX hashes to give a 64-bit result.
	fn twox_64(data: &[u8]) -> [u8; 8] {
		sp_core::hashing::twox_64(data)
	}
}
/// Wasm only interface that provides functions for calling into the allocator.
#[runtime_interface(wasm_only)]
trait Allocator {
	/// Malloc the given number of bytes and return the pointer to the allocated memory location.
	fn malloc(&mut self, size: u32) -> Pointer<u8> {
		self.allocate_memory(size).expect("Failed to allocate memory")
	}

	/// Free the given pointer.
	fn free(&mut self, ptr: Pointer<u8>) {
		self.deallocate_memory(ptr).expect("Failed to deallocate memory")
	}
}

/// Interface that provides functions for logging from within the runtime.
#[runtime_interface]
pub trait Logging {
	/// Request to print a log message on the host.
	///
	/// Note that this will be only displayed if the host is enabled to display log messages with
	/// given level and target.
	///
	/// Instead of using directly, prefer setting up `RuntimeLogger` and using `log` macros.
	fn log(level: LogLevel, target: &str, message: &[u8]) {
		if let Ok(message) = std::str::from_utf8(message) {
			log::log!(
				target: target,
				log::Level::from(level),
				"{}",
				message,
			)
		}
	}
}
