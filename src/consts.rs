pub const CONFIDENCE_FACTOR_CF: &str = "avail_light_confidence_factor_cf";
pub const BLOCK_HEADER_CF: &str = "avail_light_block_header_cf";
pub const BLOCK_CID_CF: &str = "avail_light_block_cid_cf";

/// Max concurrent tasks used generally by `StreamExt::buffered`.
/// Please be aware that `concurrency != parallel`.
pub const MAX_CONCURRENT_TASKS: usize = 10;
