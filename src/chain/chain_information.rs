use crate::header;

use alloc::vec::Vec;

/// Information about the latest finalized block and state found in its ancestors.
#[derive(Debug)]
pub struct ChainInformation {
    /// SCALE encoding of the header of the highest known finalized block.
    ///
    /// Once the queue is created, it is as if you had called
    /// [`NonFinalizedTree::set_finalized_block`] with this block.
    // TODO: should be an owned decoded header?
    pub finalized_block_header: Vec<u8>,

    /// If the number in [`ChainInformation::finalized_block_header`] is superior or equal to 1,
    /// then this field must contain the slot number of the block whose number is 1 and is an
    /// ancestor of the finalized block.
    pub babe_finalized_block1_slot_number: Option<u64>,

    /// Babe epoch information about the epoch the finalized block belongs to.
    ///
    /// Must be `None` if and only if the finalized block is block #0 or belongs to epoch #0.
    pub babe_finalized_block_epoch_information:
        Option<(header::BabeNextEpoch, header::BabeNextConfig)>,

    /// Babe epoch information about the epoch right after the one the finalized block belongs to.
    ///
    /// Must be `None` if and only if the finalized block is block #0.
    pub babe_finalized_next_epoch_transition:
        Option<(header::BabeNextEpoch, header::BabeNextConfig)>,

    /// Grandpa authorities set ID of the block right after finalized block.
    ///
    /// If the finalized block is the genesis, should be 0. Otherwise,
    // TODO: document how to know this
    pub grandpa_after_finalized_block_authorities_set_id: u64,

    /// List of GrandPa authorities that need to finalize the block right after the finalized
    /// block.
    pub grandpa_finalized_triggered_authorities: Vec<header::GrandpaAuthority>,

    /// List of changes in the GrandPa authorities list that have been scheduled by blocks that
    /// are already finalized but not triggered yet. These changes will for sure happen.
    pub grandpa_finalized_scheduled_changes: Vec<FinalizedScheduledChange>,
}

#[derive(Debug, Clone)]
pub struct FinalizedScheduledChange {
    pub trigger_block_height: u64,
    pub new_authorities_list: Vec<header::GrandpaAuthority>,
}

/// Equivalent to a [`ChainInformation`] but referencing an existing structure. Cheap to copy.
#[derive(Debug, Clone)]
pub struct ChainInformationRef<'a> {
    /// See equivalent field in [`ChanInformation`].
    pub finalized_block_header: &'a [u8],

    /// See equivalent field in [`ChanInformation`].
    pub babe_finalized_block1_slot_number: Option<u64>,

    /// See equivalent field in [`ChanInformation`].
    pub babe_finalized_block_epoch_information:
        Option<(header::BabeNextEpochRef<'a>, header::BabeNextConfig)>,

    /// See equivalent field in [`ChanInformation`].
    pub babe_finalized_next_epoch_transition:
        Option<(header::BabeNextEpochRef<'a>, header::BabeNextConfig)>,

    /// See equivalent field in [`ChanInformation`].
    pub grandpa_after_finalized_block_authorities_set_id: u64,

    /// See equivalent field in [`ChanInformation`].
    pub grandpa_finalized_triggered_authorities: &'a [header::GrandpaAuthority],

    /// See equivalent field in [`ChanInformation`].
    // TODO: better type, as a Vec is not in the spirit of this struct; however it's likely that this "scheduled changes" field will disappear altogether
    pub grandpa_finalized_scheduled_changes: Vec<FinalizedScheduledChange>,
}

impl<'a> From<&'a ChainInformation> for ChainInformationRef<'a> {
    fn from(info: &'a ChainInformation) -> ChainInformationRef<'a> {
        ChainInformationRef {
            finalized_block_header: &info.finalized_block_header,
            babe_finalized_block1_slot_number: info.babe_finalized_block1_slot_number,
            babe_finalized_block_epoch_information: info
                .babe_finalized_block_epoch_information
                .as_ref()
                .map(|(i, c)| (i.into(), *c)),
            babe_finalized_next_epoch_transition: info
                .babe_finalized_next_epoch_transition
                .as_ref()
                .map(|(i, c)| (i.into(), *c)),
            grandpa_after_finalized_block_authorities_set_id: info
                .grandpa_after_finalized_block_authorities_set_id,
            grandpa_finalized_triggered_authorities: &info.grandpa_finalized_triggered_authorities,
            grandpa_finalized_scheduled_changes: info.grandpa_finalized_scheduled_changes.clone(),
        }
    }
}

impl<'a> From<ChainInformationRef<'a>> for ChainInformation {
    fn from(info: ChainInformationRef<'a>) -> ChainInformation {
        ChainInformation {
            finalized_block_header: info.finalized_block_header.to_owned(),
            babe_finalized_block1_slot_number: info.babe_finalized_block1_slot_number,
            babe_finalized_block_epoch_information: info
                .babe_finalized_block_epoch_information
                .map(|(e, c)| (e.clone().into(), c)),
            babe_finalized_next_epoch_transition: info
                .babe_finalized_next_epoch_transition
                .map(|(e, c)| (e.clone().into(), c)),
            grandpa_after_finalized_block_authorities_set_id: info
                .grandpa_after_finalized_block_authorities_set_id,
            grandpa_finalized_triggered_authorities: info
                .grandpa_finalized_triggered_authorities
                .into(),
            grandpa_finalized_scheduled_changes: info.grandpa_finalized_scheduled_changes.into(),
        }
    }
}
