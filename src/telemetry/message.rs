// Copyright 2020 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

//! Contains the possible messages sent over the telemetry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// TODO: I really hate that we magically rely on H256 and `chrono::DateTime` to be serialized correctly

// TODO: stronger typing? lots of `Box<str>` there

pub use primitive_types::H256 as BlockHash;

#[derive(Serialize, Deserialize, Debug)]
pub struct MessageWrapper {
    pub level: Level,
    pub ts: DateTime<Utc>,
    #[serde(flatten)]
    pub details: TelemetryMessage,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Level {
    #[serde(rename = "INFO")]
    Info,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "msg")]
pub enum TelemetryMessage {
    #[serde(rename = "node.start")]
    NodeStart(Block),
    #[serde(rename = "system.connected")]
    SystemConnected(SystemConnected),
    #[serde(rename = "system.interval")]
    SystemInterval(SystemInterval),
    #[serde(rename = "block.import")]
    BlockImport(Block),
    #[serde(rename = "notify.finalized")]
    NotifyFinalized(Finalized),
    #[serde(rename = "afg.finalized")]
    AfgFinalized(AfgFinalized),
    #[serde(rename = "afg.received_precommit")]
    AfgReceivedPrecommit(AfgReceivedPrecommit),
    #[serde(rename = "afg.received_prevote")]
    AfgReceivedPrevote(AfgReceivedPrevote),
    #[serde(rename = "afg.received_commit")]
    AfgReceivedCommit(AfgReceivedCommit),
    #[serde(rename = "afg.authority_set")]
    AfgAuthoritySet(AfgAuthoritySet),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SystemConnected {
    pub chain: Box<str>,
    pub name: Box<str>,
    pub implementation: Box<str>,
    pub version: Box<str>,
    pub validator: Option<Box<str>>,
    pub network_id: Option<Box<str>>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SystemInterval {
    #[serde(flatten)]
    pub stats: NodeStats,
    pub memory: Option<f32>,
    pub cpu: Option<f32>,
    pub bandwidth_upload: Option<f64>,
    pub bandwidth_download: Option<f64>,
    pub finalized_height: Option<u64>,
    pub finalized_hash: Option<BlockHash>,
    #[serde(flatten)]
    pub block: Block,
    pub used_state_cache_size: Option<f32>,
    pub used_db_cache_size: Option<f32>,
    pub disk_read_per_sec: Option<f32>,
    pub disk_write_per_sec: Option<f32>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Finalized {
    #[serde(rename = "best")]
    pub hash: BlockHash,
    pub height: Box<str>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AfgAuthoritySet {
    pub authority_id: Box<str>,
    pub authorities: Box<str>,
    pub authority_set_id: Box<str>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AfgFinalized {
    pub finalized_hash: BlockHash,
    pub finalized_number: Box<str>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AfgReceived {
    pub target_hash: BlockHash,
    pub target_number: Box<str>,
    pub voter: Option<Box<str>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AfgReceivedPrecommit {
    #[serde(flatten)]
    pub received: AfgReceived,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AfgReceivedPrevote {
    #[serde(flatten)]
    pub received: AfgReceived,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AfgReceivedCommit {
    #[serde(flatten)]
    pub received: AfgReceived,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Block {
    #[serde(rename = "best")]
    pub hash: BlockHash,
    pub height: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeStats {
    pub peers: u64,
    pub txcount: u64,
}
