//! Column family names and other constants.

/// Column family for confidence factor
pub const CONFIDENCE_FACTOR_CF: &str = "avail_light_confidence_factor_cf";

/// Column family for block header
pub const BLOCK_HEADER_CF: &str = "avail_light_block_header_cf";

/// Column family for app data
pub const APP_DATA_CF: &str = "avail_light_app_data_cf";

/// Column family for state
pub const STATE_CF: &str = "avail_light_state_cf";

/// Expected network Node versions
pub const EXPECTED_SYSTEM_VERSION: &str = "1.10";
pub const EXPECTED_SPEC_NAME: &str = "data-avail";
pub struct ExpectedNodeVariant {
	pub system_version: &'static str,
	pub spec_name: &'static str,
}
impl ExpectedNodeVariant {
	pub const fn new() -> Self {
		Self {
			system_version: EXPECTED_SYSTEM_VERSION,
			spec_name: EXPECTED_SPEC_NAME,
		}
	}
}
