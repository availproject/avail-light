use super::types::Version;

pub fn version(version: String, network_version: String) -> Version {
	Version {
		version,
		network_version,
	}
}
