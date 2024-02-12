//! Column family names and other constants.

/// Column family for confidence factor
pub const CONFIDENCE_FACTOR_CF: &str = "avail_light_confidence_factor_cf";

/// Column family for block header
pub const BLOCK_HEADER_CF: &str = "avail_light_block_header_cf";

/// Column family for app data
pub const APP_DATA_CF: &str = "avail_light_app_data_cf";

/// Column family for state
pub const STATE_CF: &str = "avail_light_state_cf";

/// Expected network Node versions. First version should be the main supported version,
/// while all subsequent versions should be for backward compatibility/fallback/future-proofing versions.
pub const EXPECTED_SYSTEM_VERSION: &[&str] = &["1.10", "1.11"];
pub const EXPECTED_SPEC_NAME: &str = "data-avail";

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
