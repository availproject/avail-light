// Copyright 2017-2020 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

//! Network packet message types. These get serialized and put into the lower level protocol payload.

use crate::block::*; // TODO: precise imports

use bitflags::bitflags;
use core::fmt;
use parity_scale_codec::{
    Decode, Encode, EncodeAsRef, EncodeLike, Error, HasCompact, Input, Output,
};
use primitive_types::H256;

/// A unique ID of a request.
pub type RequestId = u64;

/// A set of transactions.
pub type Transactions = Vec<Extrinsic>;

// Bits of block data and associated artifacts to request.
bitflags! {
    /// Node roles bitmask.
    pub struct BlockAttributes: u8 {
        /// Include block header.
        const HEADER = 0b00000001;
        /// Include block body.
        const BODY = 0b00000010;
        /// Include block receipt.
        const RECEIPT = 0b00000100;
        /// Include block message queue.
        const MESSAGE_QUEUE = 0b00001000;
        /// Include a justification for the block.
        const JUSTIFICATION = 0b00010000;
    }
}

impl Encode for BlockAttributes {
    fn encode_to<T: Output>(&self, dest: &mut T) {
        dest.push_byte(self.bits())
    }
}

impl parity_scale_codec::EncodeLike for BlockAttributes {}

impl Decode for BlockAttributes {
    fn decode<I: Input>(input: &mut I) -> Result<Self, Error> {
        Self::from_bits(input.read_byte()?).ok_or_else(|| Error::from("Invalid bytes"))
    }
}

bitflags! {
    /// Bitmask of the roles that a node fulfills.
    pub struct Roles: u8 {
        /// No network.
        const NONE = 0b00000000;
        /// Full node, does not participate in consensus.
        const FULL = 0b00000001;
        /// Light client node.
        const LIGHT = 0b00000010;
        /// Act as an authority
        const AUTHORITY = 0b00000100;
    }
}

impl Roles {
    /// Does this role represents a client that holds full chain data locally?
    pub fn is_full(&self) -> bool {
        self.intersects(Roles::FULL | Roles::AUTHORITY)
    }

    /// Does this role represents a client that does not participates in the consensus?
    pub fn is_authority(&self) -> bool {
        *self == Roles::AUTHORITY
    }

    /// Does this role represents a client that does not hold full chain data locally?
    pub fn is_light(&self) -> bool {
        !self.is_full()
    }
}

impl fmt::Display for Roles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl parity_scale_codec::Encode for Roles {
    fn encode_to<T: parity_scale_codec::Output>(&self, dest: &mut T) {
        dest.push_byte(self.bits())
    }
}

impl parity_scale_codec::EncodeLike for Roles {}

impl parity_scale_codec::Decode for Roles {
    fn decode<I: parity_scale_codec::Input>(
        input: &mut I,
    ) -> Result<Self, parity_scale_codec::Error> {
        Self::from_bits(input.read_byte()?)
            .ok_or_else(|| parity_scale_codec::Error::from("Invalid bytes"))
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Encode, Decode)]
/// Block enumeration direction.
pub enum Direction {
    /// Enumerate in ascending order (from child to parent).
    Ascending = 0,
    /// Enumerate in descending order (from parent to canonical child).
    Descending = 1,
}

/// Block state in the chain.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Encode, Decode)]
pub enum BlockState {
    /// Block is not part of the best chain.
    Normal,
    /// Latest best block.
    Best,
}

/// Remote call response.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct RemoteCallResponse {
    /// Id of a request this response was made for.
    pub id: RequestId,
    /// Execution proof.
    pub proof: StorageProof,
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Remote read response.
pub struct RemoteReadResponse {
    /// Id of a request this response was made for.
    pub id: RequestId,
    /// Read proof.
    pub proof: StorageProof,
}

/// Consensus is mostly opaque to us
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct ConsensusMessage {
    /// Identifies consensus engine.
    pub engine_id: [u8; 4],
    /// Message payload.
    pub data: Vec<u8>,
}

/// Block data sent in the response.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct BlockData {
    /// Block header hash.
    pub hash: H256,
    /// Block header if requested.
    pub header: Option<Header>,
    /// Block body if requested.
    pub body: Option<Vec<Extrinsic>>,
    /// Block receipt if requested.
    pub receipt: Option<Vec<u8>>,
    /// Block message queue if requested.
    pub message_queue: Option<Vec<u8>>,
    /// Justification if requested.
    pub justification: Option<Vec<u8>>,
}

/// Identifies starting point of a block sequence.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub enum FromBlock {
    /// Start with given hash.
    Hash(H256),
    /// Start with given block number.
    Number(u64), // TODO: Must be exactly correct, which is annoying. But it doesn't matter if we're switching to the new blocks protocol
}

/// A network message.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub enum Message {
    /// Status packet.
    Status(Status),
    /// Block request.
    BlockRequest(BlockRequest),
    /// Block response.
    BlockResponse(BlockResponse),
    /// Block announce.
    BlockAnnounce(BlockAnnounce),
    /// Transactions.
    Transactions(Transactions),
    /// Consensus protocol message.
    Consensus(ConsensusMessage),
    /// Remote method call request.
    RemoteCallRequest(RemoteCallRequest),
    /// Remote method call response.
    RemoteCallResponse(RemoteCallResponse),
    /// Remote storage read request.
    RemoteReadRequest(RemoteReadRequest),
    /// Remote storage read response.
    RemoteReadResponse(RemoteReadResponse),
    /// Remote header request.
    RemoteHeaderRequest(RemoteHeaderRequest),
    /// Remote header response.
    RemoteHeaderResponse(RemoteHeaderResponse),
    /// Remote changes request.
    RemoteChangesRequest(RemoteChangesRequest),
    /// Remote changes response.
    RemoteChangesResponse(RemoteChangesResponse),
    /// Remote child storage read request.
    RemoteReadChildRequest(RemoteReadChildRequest),
    /// Finality proof request.
    FinalityProofRequest(FinalityProofRequest),
    /// Finality proof response.
    FinalityProofResponse(FinalityProofResponse),
    /// Batch of consensus protocol messages.
    ConsensusBatch(Vec<ConsensusMessage>),
    /// Chain-specific message.
    #[codec(index = "255")]
    ChainSpecific(Vec<u8>),
}

/// Status sent on connection.
// TODO https://github.com/paritytech/substrate/issues/4674: replace the `Status`
// struct with this one, after waiting a few releases beyond `NetworkSpecialization`'s
// removal (https://github.com/paritytech/substrate/pull/4665)
//
// and set MIN_VERSION to 6.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct CompactStatus {
    /// Protocol version.
    pub version: u32,
    /// Minimum supported version.
    pub min_supported_version: u32,
    /// Supported roles.
    pub roles: Roles,
    /// Best block number.
    pub best_number: u32,
    /// Best block hash.
    pub best_hash: H256,
    /// Genesis block hash.
    pub genesis_hash: H256,
}

/// Status sent on connection.
#[derive(Debug, PartialEq, Eq, Clone, Encode)]
pub struct Status {
    /// Protocol version.
    pub version: u32,
    /// Minimum supported version.
    pub min_supported_version: u32,
    /// Supported roles.
    pub roles: Roles,
    /// Best block number.
    pub best_number: u32,
    /// Best block hash.
    pub best_hash: H256,
    /// Genesis block hash.
    pub genesis_hash: H256,
    /// DEPRECATED. Chain-specific status.
    pub chain_status: Vec<u8>,
}

impl Decode for Status {
    fn decode<I: Input>(value: &mut I) -> Result<Self, parity_scale_codec::Error> {
        const LAST_CHAIN_STATUS_VERSION: u32 = 5;
        let compact = CompactStatus::decode(value)?;
        let chain_status = match <Vec<u8>>::decode(value) {
            Ok(v) => v,
            Err(e) => {
                if compact.version <= LAST_CHAIN_STATUS_VERSION {
                    return Err(e);
                } else {
                    Vec::new()
                }
            }
        };

        let CompactStatus {
            version,
            min_supported_version,
            roles,
            best_number,
            best_hash,
            genesis_hash,
        } = compact;

        Ok(Status {
            version,
            min_supported_version,
            roles,
            best_number,
            best_hash,
            genesis_hash,
            chain_status,
        })
    }
}

/// Request block data from a peer.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct BlockRequest {
    /// Unique request id.
    pub id: RequestId,
    /// Bits of block data to request.
    pub fields: BlockAttributes,
    /// Start from this block.
    pub from: FromBlock,
    /// End at this block. An implementation defined maximum is used when unspecified.
    pub to: Option<H256>,
    /// Sequence direction.
    pub direction: Direction,
    /// Maximum number of blocks to return. An implementation defined maximum is used when unspecified.
    pub max: Option<u32>,
}

/// Response to `BlockRequest`
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct BlockResponse {
    /// Id of a request this response was made for.
    pub id: RequestId,
    /// Block data for the requested sequence.
    pub blocks: Vec<BlockData>,
}

/// Announce a new complete relay chain block on the network.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct BlockAnnounce {
    /// New block header.
    pub header: Header,
    /// Block state. TODO: Remove `Option` and custom encoding when v4 becomes common.
    pub state: Option<BlockState>,
    /// Data associated with this block announcement, e.g. a candidate message.
    pub data: Option<Vec<u8>>,
}

// Custom Encode/Decode impl to maintain backwards compatibility with v3.
// This assumes that the packet contains nothing but the announcement message.
// TODO: Get rid of it once protocol v4 is common.
impl Encode for BlockAnnounce {
    fn encode_to<T: Output>(&self, dest: &mut T) {
        self.header.encode_to(dest);
        if let Some(state) = &self.state {
            state.encode_to(dest);
        }
        if let Some(data) = &self.data {
            data.encode_to(dest)
        }
    }
}

impl Decode for BlockAnnounce {
    fn decode<I: Input>(input: &mut I) -> Result<Self, parity_scale_codec::Error> {
        let header = Header::decode(input)?;
        let state = BlockState::decode(input).ok();
        let data = Vec::decode(input).ok();
        Ok(BlockAnnounce {
            header,
            state,
            data,
        })
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Remote call request.
pub struct RemoteCallRequest {
    /// Unique request id.
    pub id: RequestId,
    /// Block at which to perform call.
    pub block: H256,
    /// Method name.
    pub method: String,
    /// Call data.
    pub data: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Remote storage read request.
pub struct RemoteReadRequest {
    /// Unique request id.
    pub id: RequestId,
    /// Block at which to perform call.
    pub block: H256,
    /// Storage key.
    pub keys: Vec<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Remote storage read child request.
pub struct RemoteReadChildRequest {
    /// Unique request id.
    pub id: RequestId,
    /// Block at which to perform call.
    pub block: H256,
    /// Child Storage key.
    pub storage_key: Vec<u8>,
    /// Child trie source information.
    pub child_info: Vec<u8>,
    /// Child type, its required to resolve `child_info`
    /// content and choose child implementation.
    pub child_type: u32,
    /// Storage key.
    pub keys: Vec<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Remote header request.
pub struct RemoteHeaderRequest {
    /// Unique request id.
    pub id: RequestId,
    /// Block number to request header for.
    pub block: u32,
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Remote header response.
pub struct RemoteHeaderResponse {
    /// Id of a request this response was made for.
    pub id: RequestId,
    /// Header. None if proof generation has failed (e.g. header is unknown).
    pub header: Option<Header>,
    /// Header proof.
    pub proof: StorageProof,
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Remote changes request.
pub struct RemoteChangesRequest {
    /// Unique request id.
    pub id: RequestId,
    /// Hash of the first block of the range (including first) where changes are requested.
    pub first: H256,
    /// Hash of the last block of the range (including last) where changes are requested.
    pub last: H256,
    /// Hash of the first block for which the requester has the changes trie root. All other
    /// affected roots must be proved.
    pub min: H256,
    /// Hash of the last block that we can use when querying changes.
    pub max: H256,
    /// Storage child node key which changes are requested.
    pub storage_key: Option<Vec<u8>>,
    /// Storage key which changes are requested.
    pub key: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Remote changes response.
pub struct RemoteChangesResponse {
    /// Id of a request this response was made for.
    pub id: RequestId,
    /// Proof has been generated using block with this number as a max block. Should be
    /// less than or equal to the RemoteChangesRequest::max block number.
    pub max: u32,
    /// Changes proof.
    pub proof: Vec<Vec<u8>>,
    /// Changes tries roots missing on the requester' node.
    pub roots: Vec<(u32, H256)>,
    /// Missing changes tries roots proof.
    pub roots_proof: StorageProof,
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Finality proof request.
pub struct FinalityProofRequest {
    /// Unique request id.
    pub id: RequestId,
    /// Hash of the block to request proof for.
    pub block: H256,
    /// Additional data blob (that both requester and provider understood) required for proving finality.
    pub request: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
/// Finality proof response.
pub struct FinalityProofResponse {
    /// Id of a request this response was made for.
    pub id: RequestId,
    /// Hash of the block (the same as in the FinalityProofRequest).
    pub block: H256,
    /// Finality proof (if available).
    pub proof: Option<Vec<u8>>,
}
