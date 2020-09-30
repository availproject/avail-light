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

//! JSON-RPC servers.

// TODO: write docs ^

// TODO: remove this:
// link to old code: https://github.com/tomaka/substrate-lite/blob/fd11a4dbf3bcc39e4c7eaad5b38f0e2a1ff7ed61/src/json_rpc.rs

pub mod methods;
pub mod parse;
pub mod websocket_server;
