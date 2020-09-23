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

//! Justifications contain a proof of the finality of a block.
//!
//! In order to finalize a block, GrandPa authorities must emit votes, also called pre-commits.
//! We consider a block finalized when more than two thirds of the authorities have voted for that
//! block (or one of its descendants) to be finalized.
//!
//! A justification contains all the votes that have been used to prove that a certain block has
//! been finalized. Its purpose is to later be sent to a third party in order to prove this
//! finality.
//!
//! When a justification is received from a third party, it must first be verified. See the
//! [`verify`] module.

pub mod decode;
pub mod verify;
