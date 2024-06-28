use color_eyre::eyre::{Context, Result};
use semver::{Version, VersionReq};
use serde::Deserialize;
use std::env;
use tokio::process::{Child, Command};
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};
use tracing_subscriber::FmtSubscriber;

// Tag name represents release version and it is in format v{major}.{minor}.{patch}
// Additional labels for pre-release and build metadata are available as extensions of the tag name.
#[derive(Deserialize, Debug)]
struct GithubRelease {
	tag_name: String,
}

impl GithubRelease {
	// Strip 'v' character from tag name
	fn version(&self) -> &str {
		self.tag_name.get(1..).unwrap_or("")
	}
}

fn version_req(current_version: &Version) -> Result<VersionReq> {
	let Version { major, minor, .. } = current_version;
	VersionReq::parse(&format!("~{major}.{minor}")).context("Failed to parse version requirement")
}

// NOTE: Rate limiting: You can make unauthenticated requests if you are only fetching public data.
// Unauthenticated requests are associated with the originating IP address,
// not with the user or application that made the request.
async fn get_releases(client: &reqwest::Client) -> Result<Vec<GithubRelease>> {
	// TODO: Releases pagination
	client
		.get("https://api.github.com/repos/availproject/avail-light/releases")
		.header("Accept", "application/vnd.github+json")
		.header("X-GitHub-Api-Version", "2022-11-28")
		.header("User-Agent", "availproject/avail-light")
		.send()
		.await
		.context("Failed to get GitHub releases")?
		.json()
		.await
		.context("Failed to parse GitHub releases")
}

async fn get_current_version() -> Result<Version> {
	let output = Command::new("./avail-light")
		.arg("--version")
		.output()
		.await
		.context("Failed to start command")?;

	let current_version_value = String::from_utf8_lossy(&output.stdout)
		.replace("avail-light ", "")
		.replace('\n', "");

	Version::parse(&current_version_value).context(format!(
		"Failed to parse version from {current_version_value}"
	))
}

async fn spawn_avail_light_with_args() -> Result<Child> {
	let args: Vec<String> = env::args().skip(1).collect();
	let mut command = Command::new("./avail-light");
	command.args(&args);
	command.spawn().context("Failed to spawn avail-light")
}

// TODO: Kill avail-light process if watcher panics or fails for any reason
// - by handling result properly
// - by using child std::panic::set_hook
// - by handling sigterm and sigint signals
// - by using operating system specific features
async fn launch_and_watch(current_version: Version) -> Result<()> {
	let mut avail_light = spawn_avail_light_with_args().await?;
	let client = reqwest::Client::new();

	// The primary rate limit for unauthenticated requests is 60 requests per hour.
	// We will poll on every five minutes.
	let mut interval_timer = interval(Duration::from_secs(300));
	let version_req = version_req(&current_version)?;

	loop {
		interval_timer.tick().await;

		if let Some(version) = get_releases(&client)
			.await?
			.iter()
			.flat_map(|release| Version::parse(release.version()))
			.find(|version| version > &current_version && version_req.matches(version))
		{
			warn!("New compatible version found: {version}");
			return avail_light.kill().await.context("Failed to kill process");
		}
	}
}

// TODO: Add launcher to release process

#[tokio::main]
async fn main() {
	tracing::subscriber::set_global_default(FmtSubscriber::builder().finish())
		.expect("Tracing subscriber is set");

	let current_version = match get_current_version().await {
		Ok(version) => version,
		Err(error) => {
			error!("Failed to get current version: {error}");
			return;
		},
	};

	info!("Launching Avail Light Client v{current_version} ...");

	if let Err(error) = launch_and_watch(current_version).await {
		error!("Launcher failed: {error}");
	}
}
