//! Column family names and other constants.

use crate::rpc::ExpectedVersion;

/// Column family for confidence factor
pub const CONFIDENCE_FACTOR_CF: &str = "avail_light_confidence_factor_cf";

/// Column family for block header
pub const BLOCK_HEADER_CF: &str = "avail_light_block_header_cf";

/// Column family for app data
pub const APP_DATA_CF: &str = "avail_light_app_data_cf";

/// Column family for state
pub const STATE_CF: &str = "avail_light_state_cf";

/// Expected network version
pub const EXPECTED_NETWORK_VERSION: ExpectedVersion = ExpectedVersion {
	version: "1.7",
	spec_name: "data-avail",
};
