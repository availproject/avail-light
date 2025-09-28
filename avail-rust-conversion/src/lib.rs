use avail_rust::avail::runtime_types::avail_core::data_lookup::compact::CompactDataLookup as LegacyCompactDataLookup;
use avail_rust::avail::runtime_types::avail_core::data_lookup::compact::DataLookupItem as LegacyDataLookupItem;
use avail_rust::avail::runtime_types::avail_core::header::extension::v3::HeaderExtension as LegacyV3HeaderExtension;
use avail_rust::avail::runtime_types::avail_core::header::extension::HeaderExtension as LegacyHeaderExtension;
use avail_rust::avail::runtime_types::avail_core::kate_commitment::v3::KateCommitment as LegacyKateCommitment;
use avail_rust::subxt::ext::subxt_core::config::substrate::Digest as LegacyDigest;
use avail_rust::subxt::ext::subxt_core::config::substrate::DigestItem as LegacyDigestItem;
use avail_rust::AvailHeader as LegacyAvailHeader;

use avail_rust_next::avail_rust_core::header::DataLookupItem as NextDataLookupItem;
use avail_rust_next::avail_rust_core::CompactDataLookup as NextCompactDataLookup;
use avail_rust_next::subxt_core::config::substrate::Digest as NextDigest;
use avail_rust_next::subxt_core::config::substrate::DigestItem as NextDigestItem;
use avail_rust_next::AvailHeader as NextAvailHeader;
use avail_rust_next::HeaderExtension as NextHeaderExtension;
use avail_rust_next::KateCommitment as NextKateCommitment;
use avail_rust_next::V3HeaderExtension as NextV3HeaderExtension;

pub fn from_next_to_legacy_header(next: NextAvailHeader) -> LegacyAvailHeader {
	LegacyAvailHeader {
		parent_hash: next.parent_hash,
		number: next.number,
		state_root: next.state_root,
		extrinsics_root: next.extrinsics_root,
		digest: next_to_legacy_digest(next.digest),
		extension: next_to_legacy_header_extension(next.extension),
	}
}

fn next_to_legacy_digest(next: NextDigest) -> LegacyDigest {
	LegacyDigest {
		logs: next
			.logs
			.into_iter()
			.map(next_to_legacy_digest_item)
			.collect(),
	}
}

pub fn next_to_legacy_digest_item(next: NextDigestItem) -> LegacyDigestItem {
	match next {
		NextDigestItem::PreRuntime(x, items) => LegacyDigestItem::PreRuntime(x, items),
		NextDigestItem::Consensus(x, items) => LegacyDigestItem::Consensus(x, items),
		NextDigestItem::Seal(x, items) => LegacyDigestItem::Seal(x, items),
		NextDigestItem::Other(x) => LegacyDigestItem::Other(x),
		NextDigestItem::RuntimeEnvironmentUpdated => LegacyDigestItem::RuntimeEnvironmentUpdated,
	}
}

pub fn next_to_legacy_header_extension(next: NextHeaderExtension) -> LegacyHeaderExtension {
	match next {
		NextHeaderExtension::V3(x) => {
			LegacyHeaderExtension::V3(next_to_legacy_v3_header_extension(x))
		},
	}
}

pub fn next_to_legacy_v3_header_extension(next: NextV3HeaderExtension) -> LegacyV3HeaderExtension {
	LegacyV3HeaderExtension {
		app_lookup: next_to_legacy_compact_data_lookup(next.app_lookup),
		commitment: next_to_legacy_kate_commitment(next.commitment),
	}
}

pub fn next_to_legacy_compact_data_lookup(next: NextCompactDataLookup) -> LegacyCompactDataLookup {
	LegacyCompactDataLookup {
		size: next.size,
		index: next
			.index
			.into_iter()
			.map(next_to_legacy_data_lookup_item)
			.collect(),
	}
}

pub fn next_to_legacy_data_lookup_item(next: NextDataLookupItem) -> LegacyDataLookupItem {
	LegacyDataLookupItem {
		app_id: next.app_id.into(),
		start: next.start,
	}
}

pub fn next_to_legacy_kate_commitment(next: NextKateCommitment) -> LegacyKateCommitment {
	LegacyKateCommitment {
		rows: next.rows,
		cols: next.cols,
		commitment: next.commitment,
		data_root: next.data_root,
	}
}
