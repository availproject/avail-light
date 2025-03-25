use super::{Asset, Channel, Release, Releases, Target};
use color_eyre::{
	eyre::{bail, Context},
	Result,
};
use once_cell::sync::Lazy;
use regex::Regex;
use semver::Version;
use serde::Deserialize;

static STABLE: Lazy<Regex> =
	Lazy::new(|| Regex::new(r"^avail-light-client-v\d+\.\d+\.\d+$").unwrap());

static VERSION: Lazy<Regex> =
	Lazy::new(|| Regex::new(r"^avail-light-client-v(\d+\.\d+\.\d+)").unwrap());

#[derive(Deserialize, Debug)]
struct GithubAsset {
	name: String,
	browser_download_url: String,
}

impl GithubAsset {
	fn target(&self) -> Option<Target> {
		match self.name.as_str() {
			"avail-light-apple-arm64.tar.gz" => Some(Target::AppleArm64),
			"avail-light-apple-x86_64.tar.gz" => Some(Target::AppleX86_64),
			"avail-light-linux-amd64.tar.gz" => Some(Target::LinuxAmd64),
			"avail-light-x86_64-pc-windows-msvc.exe.tar.gz" => Some(Target::WindowsX86_64),
			_ => None,
		}
	}
}

// Tag name represents release version and it is in format v{major}.{minor}.{patch}
// Additional labels for release candidate is available as extensions of the tag name.
#[derive(Deserialize, Debug)]
struct GithubRelease {
	tag_name: String,
	published_at: String,
	assets: Vec<GithubAsset>,
}

impl GithubRelease {
	fn is_client(&self) -> bool {
		self.tag_name.starts_with("avail-light-client")
	}

	fn is_stable(&self) -> bool {
		STABLE.is_match(&self.tag_name)
	}

	fn version(&self) -> Option<String> {
		VERSION
			.captures(&self.tag_name)
			.and_then(|caps| caps.get(1))
			.map(|m| m.as_str().to_string())
	}
}

impl TryFrom<GithubRelease> for Release {
	type Error = color_eyre::Report;

	fn try_from(release: GithubRelease) -> Result<Self> {
		let channel = if release.is_stable() {
			Channel::Stable
		} else {
			Channel::ReleaseCandidate
		};

		let Some(release_version) = release.version() else {
			bail!("Failed to extract version")
		};

		let version = Version::parse(&release_version)
			.context(format!("Failed to parse version from {release_version}"))?;

		let published_at = chrono::DateTime::parse_from_rfc3339(&release.published_at)
			.context("Failed to parse release date")?
			.into();

		let assets = release
			.assets
			.into_iter()
			.filter_map(|asset| {
				asset.target().map(|target| Asset {
					target,
					url: asset.browser_download_url,
				})
			})
			.collect();

		Ok(Self {
			channel,
			version,
			published_at,
			assets,
		})
	}
}

// NOTE: Unauthenticated requests are associated with the originating IP address,
// not with the user or application that made the request. Limit is 60 requests per hour.
pub async fn get_releases(client: &reqwest::Client) -> Result<Releases> {
	// NOTE: No need for releases pagination since the latest client version should be in the latest 30 releases
	client
		.get("https://api.github.com/repos/availproject/avail-light/releases")
		.header("Accept", "application/vnd.github+json")
		.header("X-GitHub-Api-Version", "2022-11-28")
		.header("User-Agent", "availproject/avail-light")
		.send()
		.await
		.context("Failed to get GitHub releases")?
		.json::<Vec<GithubRelease>>()
		.await
		.context("Failed to parse GitHub releases")?
		.into_iter()
		.filter(|release| release.is_client())
		.map(|release| release.try_into())
		.collect::<Result<Releases>>()
}
