use crate::shutdown::Controller;
use avail_rust::{
	avail::runtime_types::{
		avail_core::header::extension::{v3, HeaderExtension},
		da_control::pallet::Call,
		da_runtime::RuntimeCall,
	},
	avail_core::{
		compact::CompactDataLookup, data_lookup::compact::DataLookupItem, AppId, DataLookup,
	},
	primitives::block::grandpa::{AuthorityId, ConsensusLog},
	subxt::config::substrate,
	AppUncheckedExtrinsic, AvailHeader, H256,
};
use codec::Decode;
use color_eyre::{
	eyre::{self, eyre, WrapErr},
	Result,
};
use futures::Future;
use tracing::{error, Instrument, Level, Subscriber};
use tracing_subscriber::{fmt::format, EnvFilter, FmtSubscriber};

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

pub fn filter_auth_set_changes(header: &AvailHeader) -> Vec<Vec<(AuthorityId, u64)>> {
	let new_auths = header
		.digest
		.logs
		.iter()
		.filter_map(|e| match &e {
			// UGHHH, why won't the b"FRNK" just work
			substrate::DigestItem::Consensus([b'F', b'R', b'N', b'K'], data) => {
				match ConsensusLog::<u32>::decode(&mut data.as_slice()) {
					Ok(ConsensusLog::ScheduledChange(x)) => Some(x.next_authorities),
					Ok(ConsensusLog::ForcedChange(_, x)) => Some(x.next_authorities),
					_ => None,
				}
			},
			_ => None,
		})
		.collect::<Vec<_>>();
	new_auths
}

pub fn install_panic_hooks(shutdown: Controller<String>) -> Result<()> {
	// initialize color-eyre hooks
	let (panic_hook, eyre_hook) = color_eyre::config::HookBuilder::default()
		.display_location_section(true)
		.display_env_section(true)
		.into_hooks();

	// install hook as global handler
	eyre_hook.install()?;

	std::panic::set_hook(Box::new(move |panic_info| {
		// trigger shutdown to stop other tasks if panic occurs
		let _ = shutdown.trigger_shutdown("Panic occurred, shuting down".to_string());

		let msg = format!("{}", panic_hook.panic_report(panic_info));
		error!("Error: {}", strip_ansi_escapes::strip_str(msg));

		#[cfg(debug_assertions)]
		{
			// better-panic stacktrace that is only enabled when debugging
			better_panic::Settings::auto()
				.most_recent_first(false)
				.lineno_suffix(true)
				.verbosity(better_panic::Verbosity::Medium)
				.create_panic_handler()(panic_info);
		}
	}));
	Ok(())
}

pub fn json_subscriber(log_level: Level) -> impl Subscriber + Send + Sync {
	FmtSubscriber::builder()
		.json()
		.with_env_filter(EnvFilter::new(format!("avail_light={log_level}")))
		.with_span_events(format::FmtSpan::CLOSE)
		.finish()
}

pub fn default_subscriber(log_level: Level) -> impl Subscriber + Send + Sync {
	FmtSubscriber::builder()
		.with_env_filter(EnvFilter::new(format!("avail_light={log_level}")))
		.with_span_events(format::FmtSpan::CLOSE)
		.finish()
}
