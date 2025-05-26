use std::{
	env,
	fs::{self, File, OpenOptions},
	io,
	path::PathBuf,
	sync::Arc,
	time::Duration,
};

use chrono::{DateTime, Utc};
use color_eyre::{
	eyre::{bail, Context},
	Result,
};
use rand::Rng;
use reqwest::Client;
use self_update::self_replace;
use semver::Version;
use tempfile::TempDir;
use tokio::{
	sync::{broadcast, Mutex},
	task::spawn_blocking,
	time::Instant,
};
use tracing::{debug, error, info};

use crate::{shutdown::Controller, types::BlockVerified, utils};

mod github;

#[derive(Clone, Debug, PartialEq)]
enum Target {
	LinuxAmd64,
	AppleArm64,
	AppleX86_64,
	WindowsX86_64,
}

impl Target {
	pub fn bin_name(&self) -> &'static str {
		match self {
			Target::AppleArm64 => "avail-light-apple-arm64",
			Target::AppleX86_64 => "avail-light-apple-x86_64",
			Target::LinuxAmd64 => "avail-light-linux-amd64",
			Target::WindowsX86_64 => "avail-light-x86_64-pc-windows-msvc.exe",
		}
	}

	fn detect() -> Option<Self> {
		match (env::consts::OS, env::consts::ARCH) {
			("macos", "aarch64") => Some(Self::AppleArm64),
			("macos", "x86_64") => Some(Self::AppleX86_64),
			("linux", "x86_64") => Some(Self::LinuxAmd64),
			("windows", "x86_64") => Some(Self::WindowsX86_64),
			_ => None,
		}
	}
}

#[derive(Clone, Debug)]
pub enum Channel {
	Stable,
	ReleaseCandidate,
}

#[derive(Clone, Debug)]
pub struct Asset {
	target: Target,
	archive_name: String,
	url: String,
}

#[derive(Clone, Debug)]
pub struct Release {
	pub channel: Channel,
	pub version: Version,
	pub published_at: DateTime<Utc>,
	pub assets: Vec<Asset>,
}

impl Release {
	pub fn target_asset(&self) -> Result<&Asset> {
		let Some(target) = Target::detect() else {
			bail!("Unknown target");
		};

		let Some(asset) = self.assets.iter().find(|asset| asset.target == target) else {
			bail!("No target release available");
		};

		Ok(asset)
	}
}

#[derive(Debug)]
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
	rng.gen_range(0..60)
}

fn format_duration(duration: Duration) -> String {
	let total = duration.as_secs();
	let hours = total / 3600;
	let minutes = (total % 3600) / 60;
	let seconds = total % 60;

	format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn create_archive(asset: &Asset, tempdir: &TempDir) -> Result<(fs::File, PathBuf)> {
	let path = tempdir.path().join(asset.archive_name.clone());
	let archive = File::create(&path)?;
	Ok((archive, path))
}

#[cfg(windows)]
pub fn extract_archive(archive_path: &PathBuf, asset_name: &str) -> Result<()> {
	use zip::ZipArchive;

	let file = File::open(archive_path)?;
	let mut archive = ZipArchive::new(file)?;

	for i in 0..archive.len() {
		let mut entry = archive.by_index(i)?;
		if entry.name() != asset_name {
			continue;
		}

		let mut out = OpenOptions::new()
			.create(true)
			.write(true)
			.truncate(true)
			.open(asset_name)?;

		io::copy(&mut entry, &mut out)?;
		break;
	}

	Ok(())
}

#[cfg(not(windows))]
pub fn extract_archive(archive_path: &PathBuf, asset_name: &str) -> Result<()> {
	use flate2::read::GzDecoder;
	use tar::Archive;

	let tarball = File::open(archive_path)?;
	let gz = GzDecoder::new(tarball);
	let mut archive = Archive::new(gz);

	for entry in archive.entries()? {
		let mut entry = entry?;
		let path = entry.path()?;
		if path.to_str() != Some(asset_name) {
			continue;
		}

		let mut out = OpenOptions::new()
			.create(true)
			.write(true)
			.truncate(true)
			.open(asset_name)?;

		io::copy(&mut entry, &mut out)?;
		break;
	}

	Ok(())
}

pub async fn run(
	version: &str,
	delay_sec: u64,
	shutdown: Controller<String>,
	mut block_receiver: broadcast::Receiver<BlockVerified>,
	restart: Arc<Mutex<bool>>,
	no_update: bool,
) -> Result<()> {
	info!("Starting updater...");

	// Check for new releases every 180 blocks (1 hour)
	const CHECK_INTERVAL: u64 = 3;

	// Use randomized delays_sec to pospone check interval and distribute the GitHub API requests
	let delay_blocks = (delay_sec % CHECK_INTERVAL) as u32;

	let version = Version::parse(version).expect("Version is valid");

	let client = Client::builder()
		.timeout(Duration::from_secs(10))
		.build()
		.context("Failed to create client")?;

	let delay = Duration::from_secs(delay_sec);
	let mut started_at = Instant::now();

	let mut newer_release: Option<Release> = None;

	loop {
		let block_num = block_receiver.recv().await.map(|block| block.block_num)?;

		if let Some(release) = newer_release.as_ref() {
			if no_update {
				continue;
			}
			if started_at.elapsed() >= delay {
				info!("Updating light client...");

				let asset = release.target_asset()?;

				debug!("Found asset for {:?} at: {}", asset.target, asset.url);

				let current_dir = env::current_dir()?;

				debug!("Current directory: {current_dir:?}");

				let tempdir = tempfile::Builder::new()
					.prefix("updater")
					.tempdir_in(&current_dir)?;

				let (mut archive, archive_path) = create_archive(asset, &tempdir)?;

				debug!("Temporary archive path: {archive_path:?}");

				let url = asset.url.clone();
				spawn_blocking(move || {
					let accept_header = "application/octet-stream".parse().unwrap();
					if let Err(error) = self_update::Download::from_url(&url)
						.set_header(reqwest::header::ACCEPT, accept_header)
						.show_progress(true)
						.download_to(&mut archive)
					{
						error!("Failed to download the Avail Light Client: {error:#}");
					}
				})
				.await?;

				info!("Downloaded new version from {}", asset.url);

				let asset_name = asset.target.bin_name();

				// extract to current directory
				extract_archive(&archive_path, asset_name)?;

				info!("Extracted new version of {asset_name}");

				let bin = current_dir.as_path().join(PathBuf::from(asset_name));
				self_replace::self_replace(bin)?;

				info!("Replaced new version of the light client");

				let mut restart = restart.lock().await;
				*restart = true;

				let message = "Light client updated, stopping...".to_string();
				if let Err(error) = shutdown.trigger_shutdown(message) {
					error!("{error:#}");
				}

				return Ok(());
			}
		}

		if newer_release.is_some() {
			continue;
		}

		if block_num % CHECK_INTERVAL as u32 != delay_blocks {
			continue;
		}

		let releases = match github::get_releases(&client).await {
			Ok(releases) => releases,
			Err(error) => {
				error!("{error:#}");
				continue;
			},
		};

		let Some(latest_release) = releases.latest_stable() else {
			error!("No latest release");
			continue;
		};

		debug!("Latest release: {}", latest_release.version);

		if latest_release.version > version {
			newer_release = Some(latest_release.clone());
			started_at = Instant::now();
			info!("Restart scheduled in {}", format_duration(delay));
		}
	}
}
