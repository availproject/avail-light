//! Column family names and other constants.

/// Expected network Node versions. First version should be the main supported version,
/// while all subsequent versions should be for backward compatibility/fallback/future-proofing versions.
pub const EXPECTED_SYSTEM_VERSION: &[&str] = &["2.0"];
pub const EXPECTED_SPEC_NAME: &str = "avail";

#[derive(Clone)]
pub struct ExpectedNodeVariant {
	pub system_version: &'static [&'static str],
	pub spec_name: &'static str,
}
impl ExpectedNodeVariant {
	pub const fn new() -> Self {
		Self {
			system_version: EXPECTED_SYSTEM_VERSION,
			spec_name: EXPECTED_SPEC_NAME,
		}
	}

	/// Checks if any of the expected versions matches provided network version.
	/// Since the light client uses subset of the node APIs, `matches` checks only prefix of a node version.
	/// This means that if expected version is `1.6`, versions `1.6.x` of the node will match.
	/// Specification name is checked for exact match.
	/// Since runtime `spec_version` can be changed with runtime upgrade, `spec_version` is removed.
	/// NOTE: Runtime compatibility check is currently not implemented.
	pub fn matches(&self, system_version: &str, spec_name: &str) -> bool {
		for supported_network_version in self.system_version {
			if system_version.starts_with(supported_network_version) && self.spec_name == spec_name
			{
				return true;
			}
		}
		false
	}
}
