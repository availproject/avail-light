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

// Note for the reader: sled hasn't been chosen by any particular reason other that it is a
// pure Rust database with an image of being robust. There is no strong committment from
// substrate-lite towards sled or any database engine at the moment.
// TODO: remove this node at some point ^
pub mod sled;
