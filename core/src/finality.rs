use std::collections::HashMap;

use avail_rust::H256;
use codec::Encode;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use sp_core::ed25519;
use sp_core::ed25519::Public;
use tracing::{info, warn};

use crate::{
	types::{GrandpaJustification, SignerMessage},
	utils::blake2_256,
};
use color_eyre::{eyre::eyre, Result};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorSet {
	pub set_id: u64,
	pub validator_set: Vec<Public>,
}

#[cfg(not(target_arch = "wasm32"))]
fn verify_signature(public_key: [u8; 32], signature: [u8; 64], message: Vec<u8>) -> bool {
	<ed25519::Pair as sp_core::Pair>::verify(
		&ed25519::Signature(signature),
		message,
		&ed25519::Public(public_key),
	)
}

#[cfg(target_arch = "wasm32")]
fn verify_signature(public_key: [u8; 32], signature: [u8; 64], message: Vec<u8>) -> bool {
	let public_key = ed25519_compact::PublicKey::from_slice(&public_key).unwrap();
	let signature = ed25519_compact::Signature::from_slice(&signature).unwrap();
	public_key.verify(message, &signature).is_ok()
}

pub fn check_finality(
	validator_set: &ValidatorSet,
	justification: &GrandpaJustification,
) -> Result<()> {
	// Make sure the validator set contains only unique validators
	let mut validator_set_clone = validator_set.clone();
	validator_set_clone.validator_set.sort();
	validator_set_clone.validator_set.dedup();

	// Make sure the justification ALSO contains only unique signers
	let mut justification_clone = justification.clone();
	justification_clone
		.commit
		.precommits
		.sort_by(|a, b| a.id.cmp(&b.id));
	justification_clone
		.commit
		.precommits
		.dedup_by(|a, b| a.id.eq(&b.id));

	// Shadow the original args
	let validator_set = &validator_set_clone;
	let justification = &justification_clone;

	let ancestry_map: HashMap<H256, H256> = justification
		.votes_ancestries
		.iter()
		.map(|e| (Encode::using_encoded(e, blake2_256).into(), e.parent_hash))
		.collect();

	if !ancestry_map.is_empty() {
		info!("Votes ancestries found, mapping: {ancestry_map:?}");
	}

	// verify all the Signatures of the Justification signs,
	// verify the hash of the block and extract all the signer addresses
	let (failed_verifications, signer_addresses): (Vec<_>, Vec<_>) = justification
		.commit
		.precommits
		.iter()
		.partition_map(|precommit| {
			// Try multiple set_id values to handle forced validator set changes
			let candidate_set_ids = [
				validator_set.set_id,
				validator_set.set_id.saturating_sub(1),
				validator_set.set_id.saturating_sub(2),
			];

			let (is_ok, used_set_id) = candidate_set_ids
				.iter()
				.find_map(|&set_id| {
					let signed_message = Encode::encode(&(
						&SignerMessage::PrecommitMessage(precommit.precommit.clone()),
						&justification.round,
						&set_id,
					));

					verify_signature(precommit.id.0, precommit.signature.0, signed_message).then(
						|| {
							if set_id != validator_set.set_id {
								info!(
									"Used set_id {} instead of expected {}",
									set_id, validator_set.set_id
								);
							}
							(true, set_id)
						},
					)
				})
				.unwrap_or((false, validator_set.set_id));

			let ancestry = confirm_ancestry(
				&precommit.precommit.target_hash,
				&justification.commit.target_hash,
				&ancestry_map,
			);

			(is_ok && ancestry)
				.then(|| precommit.clone().id)
				.ok_or((precommit, used_set_id, justification))
				.into()
		});

	for (precommit, set_id, just) in failed_verifications {
		warn!("Failed signature verifications on block {:?}, root block: {:?}, validator id: {:?}, set_id: {}, round: {}",
		precommit.precommit.target_hash,
		just.commit.target_hash,
		precommit.id,
		set_id,
		just.round);
	}

	// match all the Signer addresses to the Current Validator Set
	let num_matched_addresses = signer_addresses
		.iter()
		.filter(|x| validator_set.validator_set.iter().any(|e| e.0.eq(&x.0)))
		.count();

	info!(
		"Number of matching signatures: {num_matched_addresses}/{} for block {}, set_id {}",
		validator_set.validator_set.len(),
		justification.commit.target_number,
		validator_set.set_id
	);

	is_signed_by_supermajority(num_matched_addresses, validator_set.validator_set.len())
		.then_some(())
		.ok_or(eyre!("Not signed by supermajority of validator set!"))
}

fn is_signed_by_supermajority(num_signatures: usize, validator_set_size: usize) -> bool {
	let supermajority = (validator_set_size * 2 / 3) + 1;
	num_signatures >= supermajority
}

fn confirm_ancestry(
	child_hash: &H256,
	root_hash: &H256,
	ancestry_map: &HashMap<H256, H256>,
) -> bool {
	if child_hash == root_hash {
		return true;
	}

	let mut curr_hash = child_hash;

	// We should be able to test it in at most ancestry_map.len() passes
	for _ in 0..ancestry_map.len() {
		if let Some(parent_hash) = ancestry_map.get(curr_hash) {
			if parent_hash == root_hash {
				return true;
			}
			curr_hash = parent_hash;
		} else {
			return false;
		}
	}

	false
}

#[cfg(test)]
mod tests {
	use super::{check_finality, ValidatorSet};
	use crate::types::{Commit, GrandpaJustification, Precommit, SignerMessage};
	use avail_rust::AvailHeader;
	use codec::Encode;
	use hex::FromHex;
	use serde::{Deserialize, Serialize};
	use sp_core::{
		ed25519::{self, Public, Signature},
		Pair as PairT,
	};
	use std::fs::File;
	use test_case::test_case;

	#[derive(Clone, Debug, Serialize, Deserialize)]
	pub struct JsonGrandpaJustification {
		pub round: u64,
		pub commit: Commit,
		pub votes_ancestries: Vec<AvailHeader>,
	}

	impl From<GrandpaJustification> for JsonGrandpaJustification {
		fn from(value: GrandpaJustification) -> Self {
			Self {
				round: value.round,
				commit: value.commit,
				votes_ancestries: value.votes_ancestries,
			}
		}
	}

	impl From<JsonGrandpaJustification> for GrandpaJustification {
		fn from(val: JsonGrandpaJustification) -> Self {
			GrandpaJustification {
				round: val.round,
				commit: val.commit,
				votes_ancestries: val.votes_ancestries,
			}
		}
	}

	#[derive(Clone, Debug, Serialize, Deserialize)]
	pub struct ValidatorSetAndJustification {
		pub validator_set: ValidatorSet,
		pub justification: JsonGrandpaJustification,
	}

	#[test_case(1, 1 => true)]
	#[test_case(1, 2 => false)]
	#[test_case(2, 2 => true)]
	#[test_case(2, 3 => false)]
	#[test_case(3, 3 => true)]
	#[test_case(3, 4 => true)]
	#[test_case(4, 5 => true)]
	#[test_case(66, 100 => false)]
	#[test_case(67, 100 => true)]
	fn check_supermajority_condition(num_signatures: usize, validator_set_size: usize) -> bool {
		use super::is_signed_by_supermajority;
		is_signed_by_supermajority(num_signatures, validator_set_size)
	}

	#[test_case("019150591418c44041725fc53bbe69fdfb5ec4ad7c35fa3f680db07f41e096988ac3fe0314ca9829fa44fc29e5507bd56f5fa4c45fc955030309bb662f70a10e", "f55c915b3e25a013931f5401a22c3481123584d9ce5a119cabf353bca5c43f05", 41911, "0501c3f8cbba5745aa58ff5f4d8dea89fc2326aa0c95d3eb6fb8070d77511ba9", 14, 9649   => true)]
	#[test_case("b7d22a1854a4836f3d4e7f1af03f8d762913afcf2aa5b20dbdfd23af3e046e80d7410281fdb185b820687a7abe1d201ff866759b00ed2cfc0bab210cea1f7b07", "b91026ef68a88f5ab767a2a7386ac0e7dbb4e62220df1f1c865595bf3afc990b", 39863, "07bc6fca05724fb6cac16fedb80688185ddc74746c7105bddb871cccc626e5e0", 12, 8628   => true)]
	#[test_case("f5a0393906f81082fe03f74eba1a403ce3d39596b0a74f25962d6c1bb2cfe351506a5d73de8d57ff9d0813234b17273f9ee955a95b69dbba5da5c80a8783990a", "97a44517c9cf63c57b71ed76470e46c83c709cdad1c5f443e584724f73b3ab50", 423568, "a9fd0c093f2ef51dbcad38f15103a1862f475b4dc35f3bc796aad1d7cad3364f", 188, 18122   => true)]

	fn check_sig(
		sig_hex: &str,
		target_hash_hex: &str,
		target_number: u32,
		sig_id_hex: &str,
		set_id: u64,
		round: u64,
	) -> bool {
		let sig = Signature(<[u8; 64]>::from_hex(sig_hex).unwrap());
		let precommit_message = Precommit {
			target_hash: <[u8; 32]>::from_hex(target_hash_hex).unwrap().into(),
			target_number,
		};
		let id: Public = Public(<[u8; 32]>::from_hex(sig_id_hex).unwrap());
		let signed_message = Encode::encode(&(
			&SignerMessage::PrecommitMessage(precommit_message),
			&round,
			&set_id,
		));

		<ed25519::Pair as PairT>::verify(&sig, signed_message, &id)
	}

	#[test_case("src/test_assets/ancestry.json" => true; "Complex ancestry")]
	#[test_case("src/test_assets/ancestry_missing_link_no_majority.json" => false; "Missing ancestor negative case")]
	#[test_case("src/test_assets/ancestry_missing_link_works.json" => true; "Missing ancestor")]
	fn finality_test(json_path: &str) -> bool {
		let valjust_file = File::open(json_path).unwrap();
		let valjust: ValidatorSetAndJustification = serde_json::from_reader(valjust_file).unwrap();

		check_finality(&valjust.validator_set, &valjust.justification.into()).is_ok()
	}
}
