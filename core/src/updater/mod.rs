use std::time::Duration;

use chrono::{DateTime, Utc};
use color_eyre::{eyre::Context, Result};
use rand::Rng;
use reqwest::Client;
use semver::Version;
use tokio::{sync::broadcast, time::Instant};
use tracing::{error, info, warn};

use crate::{shutdown::Controller, types::BlockVerified, utils};

mod github;

pub enum Channel {
	Stable,
	ReleaseCandidate,
}

pub struct Release {
	pub channel: Channel,
	pub version: Version,
	pub published_at: DateTime<Utc>,
}

pub struct Releases(Vec<Release>);

impl Releases {
	/// Finds the latest stable release.
	pub fn latest_stable(&self) -> Option<&Release> {
		self.0
			.iter()
			.filter(|release| matches!(release.channel, Channel::Stable))
			.max_by(|a, b| a.version.cmp(&b.version))
	}
}

impl FromIterator<Release> for Releases {
	fn from_iter<I: IntoIterator<Item = Release>>(iter: I) -> Self {
		Releases(iter.into_iter().collect())
	}
}

pub fn delay_sec() -> u64 {
	// Random delay in seconds in the one day range
	let mut rng = utils::rng();
	rng.gen_range(0..60 * 60 * 24)
}

pub async fn run(
	version: &str,
	delay_sec: u64,
	shutdown: Controller<String>,
	mut block_receiver: broadcast::Receiver<BlockVerified>,
) -> Result<()> {
	info!("Starting updater...");

	let version = Version::parse(version).expect("Version is valid");

	let client = Client::builder()
		.timeout(Duration::from_secs(10))
		.build()
		.context("Failed to create client")?;

	let mut restart_scheduled = false;

	let started_at = Instant::now();

	loop {
		let block_num = match block_receiver.recv().await {
			Ok(block) => block.block_num,
			Err(error) => {
				error!("Error receiving block: {error}");
				continue;
			},
		};

		if restart_scheduled && started_at.elapsed().as_secs() >= delay_sec {
			let message = "Avail Light Client update is available, stopping...".to_string();
			if let Err(error) = shutdown.trigger_shutdown(message) {
				error!("{error:#}");
			}
			return Ok(());
		}

		// Check for new releases every 15 blocks (5 minutes)
		if restart_scheduled || block_num % 15 != 0 {
			continue;
		}

		let releases = match github::get_releases(&client).await {
			Ok(releases) => releases,
			Err(error) => {
				error!("{error:#}");
				return Ok(());
			},
		};

		let Some(latest_release) = releases.latest_stable() else {
			warn!("No latest release");
			continue;
		};

		warn!(
			"Latest release: {}, {restart_scheduled}",
			latest_release.version
		);

		restart_scheduled = latest_release.version > version;
	}
}
