use crate::shutdown::Controller;
use avail_core::{
	compact::CompactDataLookup, data_lookup::compact::DataLookupItem, AppId, DataLookup,
};
use avail_rust::{
	avail::runtime_types::{
		avail_core::header::extension::{v3, HeaderExtension},
		da_control::pallet::Call,
		da_runtime::RuntimeCall,
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
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use tokio::task::JoinHandle;
#[cfg(target_arch = "wasm32")]
use tokio_with_wasm::alias as tokio;
use tracing::{error, warn, Instrument, Level, Subscriber};
#[cfg(not(target_arch = "wasm32"))]
use tracing_error::ErrorLayer;
#[cfg(not(target_arch = "wasm32"))]
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{fmt::format, EnvFilter, FmtSubscriber};

#[cfg(feature = "multiproof")]
use kate_recovery::matrix::Dimensions;

pub fn spawn_in_span<F>(future: F) -> JoinHandle<F::Output>
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

// TODO: Remove attribute after enabling WASM on the light client
#[allow(dead_code)]
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

// TODO: Remove attribute after enabling WASM on the light client
#[allow(dead_code)]
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

pub fn filter_auth_set_changes(header: &AvailHeader) -> Vec<(AuthorityId, u64)> {
	const FRNK: [u8; 4] = *b"FRNK";

	let mut forced_valset = header
		.digest
		.logs
		.iter()
		.filter_map(|e| match &e {
			substrate::DigestItem::Consensus(FRNK, data) => {
				match ConsensusLog::<u32>::decode(&mut data.as_slice()) {
					Ok(ConsensusLog::ForcedChange(_, x)) => Some(x.next_authorities),
					_ => None,
				}
			},
			_ => None,
		})
		.collect::<Vec<_>>();

	if !forced_valset.is_empty() {
		assert!(
			forced_valset.len() == 1,
			"There should be only one forced valset change per header!"
		);
		warn!("Forced validator set change occurred, prioritizing to any co-occurring scheduled validator set change.");
		return forced_valset.pop().unwrap();
	}

	let mut scheduled_valset = header
		.digest
		.logs
		.iter()
		.filter_map(|e| match &e {
			substrate::DigestItem::Consensus(FRNK, data) => {
				match ConsensusLog::<u32>::decode(&mut data.as_slice()) {
					Ok(ConsensusLog::ScheduledChange(x)) => Some(x.next_authorities),
					_ => None,
				}
			},
			_ => None,
		})
		.collect::<Vec<_>>();

	if !scheduled_valset.is_empty() {
		assert!(
			scheduled_valset.len() == 1,
			"There should be only one scheduled valset change per header!"
		);
		return scheduled_valset.pop().unwrap();
	};

	vec![]
}

pub fn install_panic_hooks(shutdown: Controller<String>) -> Result<()> {
	#[cfg(not(target_arch = "wasm32"))]
	// initialize color-eyre hooks
	let (panic_hook, eyre_hook) = color_eyre::config::HookBuilder::default()
		.display_location_section(true)
		.display_env_section(true)
		.into_hooks();

	#[cfg(target_arch = "wasm32")]
	// initialize color-eyre hooks
	let (panic_hook, eyre_hook) = color_eyre::config::HookBuilder::default()
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
	let subscriber = FmtSubscriber::builder()
		.json()
		.with_env_filter(EnvFilter::new(format!("avail_light={log_level},warn")))
		.with_span_events(format::FmtSpan::CLOSE)
		.finish();

	#[cfg(not(target_arch = "wasm32"))]
	{
		subscriber.with(ErrorLayer::default())
	}
	#[cfg(target_arch = "wasm32")]
	{
		subscriber
	}
}

pub fn default_subscriber(log_level: Level) -> impl Subscriber + Send + Sync {
	let subscriber = FmtSubscriber::builder()
		.with_env_filter(EnvFilter::new(format!("avail_light={log_level},warn")))
		.with_span_events(format::FmtSpan::CLOSE)
		.finish();

	#[cfg(not(target_arch = "wasm32"))]
	{
		subscriber.with(ErrorLayer::default())
	}
	#[cfg(target_arch = "wasm32")]
	{
		subscriber
	}
}

#[cfg(target_arch = "wasm32")]
fn blake2<const N: usize>(data: &[u8]) -> [u8; N] {
	blake2b_simd::Params::new()
		.hash_length(N)
		.hash(data)
		.as_bytes()
		.try_into()
		.expect("slice is always the necessary length")
}

#[cfg(target_arch = "wasm32")]
pub fn blake2_256(data: &[u8]) -> [u8; 32] {
	blake2(data)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn blake2_256(data: &[u8]) -> [u8; 32] {
	sp_core::blake2_256(data)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn rng() -> rand::rngs::ThreadRng {
	rand::thread_rng()
}

#[cfg(target_arch = "wasm32")]
pub fn rng() -> rand::rngs::StdRng {
	use rand::SeedableRng;
	let mut seed = [0u8; 32];
	getrandom::getrandom(&mut seed).expect("Failed to get random seed");
	rand::rngs::StdRng::from_seed(seed)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn restart(current_exe: PathBuf) {
	#[cfg(unix)]
	{
		use std::os::unix::process::CommandExt;
		let error = std::process::Command::new(current_exe)
			.args(std::env::args().skip(1))
			.exec();
		error!(%error, "Restarting light client failed");
		std::process::exit(1);
	}

	#[cfg(windows)]
	{
		std::process::Command::new(current_exe)
			.args(std::env::args().skip(1))
			.spawn()
			.expect("Failed to restart");
		std::process::exit(0);
	}
}

#[cfg(feature = "multiproof")]
pub(crate) fn generate_multiproof_grid_dims(
	grid: Dimensions,
	target: Dimensions,
) -> Option<Dimensions> {
	let cols = core::cmp::min(grid.cols(), target.cols());
	let rows = core::cmp::min(grid.rows(), target.rows());
	if grid.cols().get() % cols != 0 || grid.rows().get() % rows != 0 {
		return None;
	}

	Dimensions::new(rows, cols)
}

pub(crate) fn is_flush_cycle(block_number: u32) -> bool {
	block_number.is_multiple_of(METRICS_FLUSH_INTERVAL)
}

const METRICS_FLUSH_INTERVAL: u32 = 5;
