// Smoldot
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
declare class SmoldotError extends Error {
  constructor(message: string);
}

export interface SmoldotClient {
  send_json_rpc(rpc: string): void;
}

export type SmoldotJsonRpcCallback = (response: string) => void;
export type SmoldotDatabaseSaveCallback = (response: string) => void;

export interface SmoldotOptions {
  max_log_level?: number;
  chain_spec: string;
  json_rpc_callback: SmoldotJsonRpcCallback;
  database_save_callback: SmoldotDatabaseSaveCallback;
  database_content?: string;
  relay_chain_spec?: string;
}

export interface Smoldot {
  start(options: SmoldotOptions): Promise<SmoldotClient>;
}

export const smoldot: Smoldot;

export default smoldot;
