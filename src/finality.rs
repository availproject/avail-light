use std::collections::HashMap;

use codec::Encode;
use sp_core::{
	blake2_256,
	ed25519::{self, Public},
	Pair, H256,
};
use tracing::{info, warn};

use crate::types::{GrandpaJustification, SignerMessage};
use color_eyre::{eyre::eyre, Result};

#[derive(Clone, Debug)]
pub struct ValidatorSet {
	pub set_id: u64,
	pub validator_set: Vec<Public>,
}

pub fn check_finality(
	validator_set: &ValidatorSet,
	justification: &GrandpaJustification,
) -> Result<()> {
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
	let signer_addresses = justification
		.commit
		.precommits
		.iter()
		.map(|precommit| {
			// form a message which is signed in the Justification, it's a triplet of a Precommit,
			// round number and set_id (taken from Substrate code)
			let signed_message = Encode::encode(&(
				&SignerMessage::PrecommitMessage(precommit.precommit.clone()),
				&justification.round,
				&validator_set.set_id, // Set ID is needed here.
			));
			let mut is_ok = <ed25519::Pair as Pair>::verify(
				&precommit.signature,
				signed_message,
				&precommit.id,
			);
			if !is_ok {
				warn!(
					"Signature verification fails with default set_id {}, trying alternatives.",
					validator_set.set_id
				);
				for set_id_m in (validator_set.set_id - 10)..(validator_set.set_id + 10) {
					let s_m = Encode::encode(&(
						&SignerMessage::PrecommitMessage(precommit.precommit.clone()),
						&justification.round,
						&set_id_m,
					));
					is_ok =
						<ed25519::Pair as Pair>::verify(&precommit.signature, &s_m, &precommit.id);
					if is_ok {
						info!("Signature match with set_id={set_id_m}");
						break;
					}
				}
			}

			let ancestry = confirm_ancestry(
				&precommit.precommit.target_hash,
				&justification.commit.target_hash,
				&ancestry_map,
			);
			(is_ok && ancestry)
				.then(|| precommit.clone().id)
				.ok_or_else(|| {
					eyre!(
				"Not signed by this signature! Sig id: {:?}, set_id: {}, justification: {:?}",
				&precommit.id,
				validator_set.set_id,
				justification
			)
				})
		})
		.collect::<Result<Vec<_>>>();

	// match all the Signer addresses to the Current Validator Set
	let num_matched_addresses = signer_addresses?
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
	use codec::Encode;
	use hex::FromHex;
	use sp_core::{
		ed25519::{self, Public, Signature},
		Pair,
	};
	use test_case::test_case;

	use crate::types::{Precommit, SignerMessage};
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

		<ed25519::Pair as Pair>::verify(&sig, signed_message, &id)
	}
}
