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

use blake2::digest::{Input as _, VariableOutput as _};
use bitflags::bitflags;
use core::fmt;
use parity_scale_codec::{
    Decode, Encode, EncodeAsRef, EncodeLike, Error, HasCompact, Input, Output,
};
use primitive_types::{H256, U256};

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
/// Simple blob to hold an extrinsic without committing to its format and ensure it is serialized
/// correctly.
#[derive(Debug, PartialEq, Eq, Clone, Default, Encode, Decode)]
pub struct Extrinsic(pub Vec<u8>);

/// Abstraction over a block header for a substrate chain.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Header {
    /// The parent hash.
    pub parent_hash: U256,
    /// The block number.
    pub number: u32,
    /// The state trie merkle root
    pub state_root: U256,
    /// The merkle root of the extrinsics.
    pub extrinsics_root: U256,
    /// A chain-specific digest of data useful for light clients or referencing auxiliary data.
    pub digest: Digest<H256>,
}

impl Header {
    /// Returns the hash of the header, and thus also of the block.
    pub fn block_hash(&self) -> BlockHash {
        let mut out = [0; 32];
        blake2_256_into(&self.encode(), &mut out);
        BlockHash(out)
    }
}

/// Hash of a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockHash(pub [u8; 32]);

/// Do a Blake2 256-bit hash and place result in `dest`.
fn blake2_256_into(data: &[u8], dest: &mut [u8; 32]) {
    let mut hasher = blake2::VarBlake2b::new_keyed(&[], 32);
    hasher.input(data);
    let result = hasher.vec_result();
    assert_eq!(result.len(), 32);
    dest.copy_from_slice(&result);
}

impl Decode for Header {
    fn decode<I: Input>(input: &mut I) -> Result<Self, Error> {
        Ok(Header {
            parent_hash: Decode::decode(input)?,
            number: <<u32 as HasCompact>::Type>::decode(input)?.into(),
            state_root: Decode::decode(input)?,
            extrinsics_root: Decode::decode(input)?,
            digest: Decode::decode(input)?,
        })
    }
}

impl Encode for Header {
    fn encode_to<T: Output>(&self, dest: &mut T) {
        dest.push(&self.parent_hash);
        dest.push(&<<<u32 as HasCompact>::Type as EncodeAsRef<_>>::RefType>::from(&self.number));
        dest.push(&self.state_root);
        dest.push(&self.extrinsics_root);
        dest.push(&self.digest);
    }
}

/// Generic header digest.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct Digest<Hash: Encode + Decode> {
    /// A list of logs in the digest.
    pub logs: Vec<DigestItem<Hash>>,
}

/// Digest item that is able to encode/decode 'system' digest items and
/// provide opaque access to other items.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum DigestItem<Hash> {
    /// System digest item that contains the root of changes trie at given
    /// block. It is created for every block iff runtime supports changes
    /// trie creation.
    ChangesTrieRoot(Hash),

    /// A pre-runtime digest.
    ///
    /// These are messages from the consensus engine to the runtime, although
    /// the consensus engine can (and should) read them itself to avoid
    /// code and state duplication. It is erroneous for a runtime to produce
    /// these, but this is not (yet) checked.
    ///
    /// NOTE: the runtime is not allowed to panic or fail in an `on_initialize`
    /// call if an expected `PreRuntime` digest is not present. It is the
    /// responsibility of a external block verifier to check this. Runtime API calls
    /// will initialize the block without pre-runtime digests, so initialization
    /// cannot fail when they are missing.
    PreRuntime([u8; 4], Vec<u8>),

    /// A message from the runtime to the consensus engine. This should *never*
    /// be generated by the native code of any consensus engine, but this is not
    /// checked (yet).
    Consensus([u8; 4], Vec<u8>),

    /// Put a Seal on it. This is only used by native code, and is never seen
    /// by runtimes.
    Seal([u8; 4], Vec<u8>),

    /// Digest item that contains signal from changes tries manager to the
    /// native code.
    ChangesTrieSignal(ChangesTrieSignal),

    /// Some other thing. Unsupported and experimental.
    Other(Vec<u8>),
}

impl<Hash> DigestItem<Hash> {
    /// Returns a 'referencing view' for this digest item.
    pub fn dref<'a>(&'a self) -> DigestItemRef<'a, Hash> {
        match *self {
            DigestItem::ChangesTrieRoot(ref v) => DigestItemRef::ChangesTrieRoot(v),
            DigestItem::PreRuntime(ref v, ref s) => DigestItemRef::PreRuntime(v, s),
            DigestItem::Consensus(ref v, ref s) => DigestItemRef::Consensus(v, s),
            DigestItem::Seal(ref v, ref s) => DigestItemRef::Seal(v, s),
            DigestItem::ChangesTrieSignal(ref s) => DigestItemRef::ChangesTrieSignal(s),
            DigestItem::Other(ref v) => DigestItemRef::Other(v),
        }
    }
}

/// A 'referencing view' for digest item. Does not own its contents. Used by
/// final runtime implementations for encoding/decoding its log items.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum DigestItemRef<'a, Hash: 'a> {
    /// Reference to `DigestItem::ChangesTrieRoot`.
    ChangesTrieRoot(&'a Hash),
    /// A pre-runtime digest.
    ///
    /// These are messages from the consensus engine to the runtime, although
    /// the consensus engine can (and should) read them itself to avoid
    /// code and state duplication.  It is erroneous for a runtime to produce
    /// these, but this is not (yet) checked.
    PreRuntime(&'a [u8; 4], &'a Vec<u8>),
    /// A message from the runtime to the consensus engine. This should *never*
    /// be generated by the native code of any consensus engine, but this is not
    /// checked (yet).
    Consensus(&'a [u8; 4], &'a Vec<u8>),
    /// Put a Seal on it. This is only used by native code, and is never seen
    /// by runtimes.
    Seal(&'a [u8; 4], &'a Vec<u8>),
    /// Digest item that contains signal from changes tries manager to the
    /// native code.
    ChangesTrieSignal(&'a ChangesTrieSignal),
    /// Any 'non-system' digest item, opaque to the native code.
    Other(&'a Vec<u8>),
}

impl<'a, Hash: Encode> Encode for DigestItemRef<'a, Hash> {
    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::new();

        match *self {
            DigestItemRef::ChangesTrieRoot(changes_trie_root) => {
                DigestItemType::ChangesTrieRoot.encode_to(&mut v);
                changes_trie_root.encode_to(&mut v);
            }
            DigestItemRef::Consensus(val, data) => {
                DigestItemType::Consensus.encode_to(&mut v);
                (val, data).encode_to(&mut v);
            }
            DigestItemRef::Seal(val, sig) => {
                DigestItemType::Seal.encode_to(&mut v);
                (val, sig).encode_to(&mut v);
            }
            DigestItemRef::PreRuntime(val, data) => {
                DigestItemType::PreRuntime.encode_to(&mut v);
                (val, data).encode_to(&mut v);
            }
            DigestItemRef::ChangesTrieSignal(changes_trie_signal) => {
                DigestItemType::ChangesTrieSignal.encode_to(&mut v);
                changes_trie_signal.encode_to(&mut v);
            }
            DigestItemRef::Other(val) => {
                DigestItemType::Other.encode_to(&mut v);
                val.encode_to(&mut v);
            }
        }

        v
    }
}

impl<'a, Hash: Encode> EncodeLike for DigestItemRef<'a, Hash> {}

impl<Hash: Encode> Encode for DigestItem<Hash> {
    fn encode(&self) -> Vec<u8> {
        self.dref().encode()
    }
}

impl<Hash: Encode> EncodeLike for DigestItem<Hash> {}

impl<Hash: Decode> Decode for DigestItem<Hash> {
    #[allow(deprecated)]
    fn decode<I: Input>(input: &mut I) -> Result<Self, Error> {
        let item_type: DigestItemType = Decode::decode(input)?;
        match item_type {
            DigestItemType::ChangesTrieRoot => {
                Ok(DigestItem::ChangesTrieRoot(Decode::decode(input)?))
            }
            DigestItemType::PreRuntime => {
                let vals: ([u8; 4], Vec<u8>) = Decode::decode(input)?;
                Ok(DigestItem::PreRuntime(vals.0, vals.1))
            }
            DigestItemType::Consensus => {
                let vals: ([u8; 4], Vec<u8>) = Decode::decode(input)?;
                Ok(DigestItem::Consensus(vals.0, vals.1))
            }
            DigestItemType::Seal => {
                let vals: ([u8; 4], Vec<u8>) = Decode::decode(input)?;
                Ok(DigestItem::Seal(vals.0, vals.1))
            }
            DigestItemType::ChangesTrieSignal => {
                Ok(DigestItem::ChangesTrieSignal(Decode::decode(input)?))
            }
            DigestItemType::Other => Ok(DigestItem::Other(Decode::decode(input)?)),
        }
    }
}

/// Type of the digest item. Used to gain explicit control over `DigestItem` encoding
/// process. We need an explicit control, because final runtimes are encoding their own
/// digest items using `DigestItemRef` type and we can't auto-derive `Decode`
/// trait for `DigestItemRef`.
#[repr(u32)]
#[derive(Encode, Decode)]
pub enum DigestItemType {
    Other = 0,
    ChangesTrieRoot = 2,
    Consensus = 4,
    Seal = 5,
    PreRuntime = 6,
    ChangesTrieSignal = 7,
}

/// Available changes trie signals.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub enum ChangesTrieSignal {
    /// New changes trie configuration is enacted, starting from **next block**.
    ///
    /// The block that emits this signal will contain changes trie (CT) that covers
    /// blocks range [BEGIN; current block], where BEGIN is (order matters):
    /// - LAST_TOP_LEVEL_DIGEST_BLOCK+1 if top level digest CT has ever been created
    ///   using current configuration AND the last top level digest CT has been created
    ///   at block LAST_TOP_LEVEL_DIGEST_BLOCK;
    /// - LAST_CONFIGURATION_CHANGE_BLOCK+1 if there has been CT configuration change
    ///   before and the last configuration change happened at block
    ///   LAST_CONFIGURATION_CHANGE_BLOCK;
    /// - 1 otherwise.
    NewConfiguration(Option<ChangesTrieConfiguration>),
}

/// Substrate changes trie configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Encode, Decode)]
pub struct ChangesTrieConfiguration {
    /// Interval (in blocks) at which level1-digests are created. Digests are not
    /// created when this is less or equal to 1.
    pub digest_interval: u32,
    /// Maximal number of digest levels in hierarchy. 0 means that digests are not
    /// created at all (even level1 digests). 1 means only level1-digests are created.
    /// 2 means that every digest_interval^2 there will be a level2-digest, and so on.
    /// Please ensure that maximum digest interval (i.e. digest_interval^digest_levels)
    /// is within `u32` limits. Otherwise you'll never see digests covering such intervals
    /// && maximal digests interval will be truncated to the last interval that fits
    /// `u32` limits.
    pub digest_levels: u32,
}

/// Abstraction over a substrate block.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct Block {
    /// The block header.
    pub header: Header,
    /// The accompanying extrinsics.
    pub extrinsics: Vec<Extrinsic>,
}

impl Block {
    /// Returns the hash of the block.
    pub fn block_hash(&self) -> BlockHash {
        self.header.block_hash()
    }
}

/// A proof that some set of key-value pairs are included in the storage trie. The proof contains
/// the storage values so that the partial storage backend can be reconstructed by a verifier that
/// does not already have access to the key-value pairs.
///
/// The proof consists of the set of serialized nodes in the storage trie accessed when looking up
/// the keys covered by the proof. Verifying the proof requires constructing the partial trie from
/// the serialized nodes and performing the key lookups.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct StorageProof {
    trie_nodes: Vec<Vec<u8>>,
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
    Number(u32),
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
