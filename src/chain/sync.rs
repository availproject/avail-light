// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Syncing, short for synchronizing, consists in synchronizing the state of a local chain with
//! the state of the chain contained in other machines, called *remotes* or *sources*.
//!
//! > **Note**: While the above summary is a bit abstract, in practice it is almost always
//! >           done by exchanging messages through a peer-to-peer network.
//!
//! Multiple strategies exist for syncing, and which one to employ depends on the amount of
//! information that is desired (e.g. is it required to know the header and/or body of every
//! single block, or can some blocks be skipped?) and the distance between the highest block of
//! the local chain and the highest block available on the remotes.
//!
//! # About safety
//!
//! While there exists various trade-offs between syncing strategies, safety is never part of
//! these trade-offs. All syncing strategies are safe, in the sense that malicious remotes cannot
//! corrupt the state of the local chain.
//!
//! It is possible, however, for a malicious remote to omit some information, such as the
//! existence of a specific block. If all the remotes that are being synchronized from are
//! malicious and collude to omit the same information, there is no way for the local node to
//! learn this information or even to be aware that it is missing an information. This is called
//! an **eclipse attack**.
//!
//! For this reason, it is important to ensure a large number and a good distribution of the
//! sources. In the context of a peer-to-peer network where machines are picked randomly, a
//! minimum threshold of around 7 peers is generally considered acceptable. Similarly, in the
//! context of a peer-to-peer network, it is important to establish outgoing connections to other
//! nodes and not only rely on incoming connections, as there is otherwise the possibility of a
//! single actor controlling all said incoming connections.

// TODO: this doc ^ is correct but assumes that there exist multiple syncing strategies, while at
//       the time of writing only one is implemented

// TODO: pub mod full_all_forks;
pub mod full_optimistic;
pub mod headers_optimistic;
// TODO: maybe shouldn't be pub, but creates doc-link errors if private
pub mod optimistic;
