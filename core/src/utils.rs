use avail_core::{
	compact::CompactDataLookup, data_lookup::compact::DataLookupItem, AppId, DataLookup,
};
use avail_subxt::{
	api::runtime_types::{
		avail_core::{header::extension::v3, header::extension::HeaderExtension},
		da_control::pallet::Call,
		da_runtime::RuntimeCall,
	},
	primitives::{
		grandpa::AuthorityId, grandpa::ConsensusLog, AppUncheckedExtrinsic, Header as DaHeader,
	},
	utils::H256,
};
use codec::Decode;
use color_eyre::{
	eyre::{self, eyre, WrapErr},
	Result,
};
use futures::Future;
use tracing::Instrument;

pub fn spawn_in_span<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
	F: Future + Send + 'static,
	F::Output: Send + 'static,
{
	tokio::spawn(future.in_current_span())
}

pub fn decode_app_data(data: &[u8]) -> Result<Option<Vec<u8>>> {
	let extrisic: AppUncheckedExtrinsic =
		<_ as Decode>::decode(&mut &data[..]).wrap_err("Couldn't decode AvailExtrinsic")?;

	match extrisic.function {
		RuntimeCall::DataAvailability(Call::submit_data { data, .. }) => Ok(Some(data.0)),
		_ => Ok(None),
	}
}

/// Calculates confidence from given number of verified cells
pub fn calculate_confidence(count: u32) -> f64 {
	100f64 * (1f64 - 1f64 / 2u32.pow(count) as f64)
}

pub trait OptionalExtension {
	fn option(&self) -> Option<&Self>;
}

impl OptionalExtension for HeaderExtension {
	fn option(&self) -> Option<&Self> {
		let HeaderExtension::V3(v3::HeaderExtension { app_lookup, .. }) = self;
		(app_lookup.size > 0).then_some(self)
	}
}

/// Extract fields from extension header
pub(crate) fn extract_kate(extension: &HeaderExtension) -> Option<(u16, u16, H256, Vec<u8>)> {
	match &extension.option()? {
		HeaderExtension::V3(v3::HeaderExtension {
			commitment: kate, ..
		}) => Some((
			kate.rows,
			kate.cols,
			kate.data_root,
			kate.commitment.clone(),
		)),
	}
}

pub(crate) fn extract_app_lookup(extension: &HeaderExtension) -> eyre::Result<Option<DataLookup>> {
	let Some(extension) = extension.option() else {
		return Ok(None);
	};

	let compact = match &extension {
		HeaderExtension::V3(v3::HeaderExtension { app_lookup, .. }) => app_lookup,
	};

	let size = compact.size;
	let index = compact
		.index
		.iter()
		.map(|item| DataLookupItem::new(AppId(item.app_id.0), item.start))
		.collect::<Vec<_>>();

	let compact = CompactDataLookup::new(size, index);
	DataLookup::try_from(compact)
		.map(Some)
		.map_err(|e| eyre!("Invalid DataLookup: {}", e))
}

pub fn filter_auth_set_changes(header: &DaHeader) -> Vec<Vec<(AuthorityId, u64)>> {
	let new_auths = header
		.digest
		.logs
		.iter()
		.filter_map(|e| match &e {
			// UGHHH, why won't the b"FRNK" just work
			avail_subxt::config::substrate::DigestItem::Consensus(
				[b'F', b'R', b'N', b'K'],
				data,
			) => match ConsensusLog::<u32>::decode(&mut data.as_slice()) {
				Ok(ConsensusLog::ScheduledChange(x)) => Some(x.next_authorities),
				Ok(ConsensusLog::ForcedChange(_, x)) => Some(x.next_authorities),
				_ => None,
			},
			_ => None,
		})
		.collect::<Vec<_>>();
	new_auths
}
