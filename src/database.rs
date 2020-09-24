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

//! Persistent data storage.
//!
//! This module contains sub-modules that provide different means of storing data in a
//! persistent way.

pub mod local_storage_light;

// TODO: when implementing an actual database for the full node, here is some inspiration:
// - https://github.com/paritytech/substrate-lite/blob/e53148ca8af7450e9995e960279b6c8925a37663/src/database/sled.rs
// - https://github.com/paritytech/substrate-lite/blob/e53148ca8af7450e9995e960279b6c8925a37663/bin/full-node.rs#L717-L758
// - https://github.com/paritytech/substrate-lite/blob/8f41c567d8dc644e2166f06f0bd5e5dfca4fddd3/src/lib.rs#L171-L208
