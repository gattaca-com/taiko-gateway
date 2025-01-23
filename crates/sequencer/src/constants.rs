/// Number of blocks to wait before refreshing the anchor, the larger this is the more blocks we can
/// fit in a batch, but we risk it getting stale.
///
/// Note: this is on top of the [`SAFE_L1_LAG`] below, so from the latest L1 blocks we keep blocks
/// up to `ANCHOR_BATCH_LAG + SAFE_L1_LAG` old
pub(crate) const ANCHOR_BATCH_LAG: u64 = 8;

/// Use as anchors blocks with this lag to make sure we dont use a reorged L1 block
pub(crate) const SAFE_L1_LAG: u64 = 4;
