// Substrate-lite
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

#![cfg(test)]

use super::{Handshake, NoiseKey};

#[test]
fn handshake_basic_works() {
    fn test_with_buffer_sizes(size1: usize, size2: usize) {
        let key1 = NoiseKey::new(&rand::random());
        let key2 = NoiseKey::new(&rand::random());

        let mut handshake1 = Handshake::new(true);
        let mut handshake2 = Handshake::new(false);

        let mut buf_1_to_2 = Vec::new();
        let mut buf_2_to_1 = Vec::new();

        while !matches!(
            (&handshake1, &handshake2),
            (Handshake::Success { .. }, Handshake::Success { .. })
        ) {
            match handshake1 {
                Handshake::Success { .. } => {}
                Handshake::NoiseKeyRequired(req) => handshake1 = req.resume(&key1).into(),
                Handshake::Healthy(nego) => {
                    if buf_1_to_2.is_empty() {
                        buf_1_to_2.resize(size1, 0);
                        let (updated, num_read, written) = nego
                            .read_write(&buf_2_to_1, (&mut buf_1_to_2, &mut []))
                            .unwrap();
                        handshake1 = updated;
                        for _ in 0..num_read {
                            buf_2_to_1.remove(0);
                        }
                        buf_1_to_2.truncate(written);
                    } else {
                        let (updated, num_read, _) =
                            nego.read_write(&buf_2_to_1, (&mut [], &mut [])).unwrap();
                        handshake1 = updated;
                        for _ in 0..num_read {
                            buf_2_to_1.remove(0);
                        }
                    }
                }
            }

            match handshake2 {
                Handshake::Success { .. } => {}
                Handshake::NoiseKeyRequired(req) => handshake2 = req.resume(&key2).into(),
                Handshake::Healthy(nego) => {
                    if buf_2_to_1.is_empty() {
                        buf_2_to_1.resize(size2, 0);
                        let (updated, num_read, written) = nego
                            .read_write(&buf_1_to_2, (&mut buf_2_to_1, &mut []))
                            .unwrap();
                        handshake2 = updated;
                        for _ in 0..num_read {
                            buf_1_to_2.remove(0);
                        }
                        buf_2_to_1.truncate(written);
                    } else {
                        let (updated, num_read, _) =
                            nego.read_write(&buf_1_to_2, (&mut [], &mut [])).unwrap();
                        handshake2 = updated;
                        for _ in 0..num_read {
                            buf_1_to_2.remove(0);
                        }
                    }
                }
            }
        }
    }

    test_with_buffer_sizes(256, 256);
    // TODO: not passing because Noise wants at least 19 bytes of buffer
    //test_with_buffer_sizes(1, 1);
    //test_with_buffer_sizes(1, 2048);
    //test_with_buffer_sizes(2048, 1);
}
