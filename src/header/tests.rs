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

#![cfg(test)]

#[test]
fn decode_rococo() {
    // Rococo block taken 2021-04-08 around 11:00 UTC.
    super::decode(&[
        5, 35, 55, 218, 117, 209, 29, 117, 103, 130, 55, 39, 55, 132, 95, 54, 138, 185, 89, 79,
        123, 161, 124, 51, 67, 40, 71, 126, 0, 210, 240, 78, 57, 177, 102, 97, 175, 183, 124, 206,
        195, 77, 217, 117, 83, 14, 134, 50, 246, 163, 138, 196, 199, 78, 108, 145, 187, 240, 123,
        5, 18, 219, 158, 44, 174, 132, 41, 70, 121, 181, 160, 189, 104, 253, 173, 135, 222, 15, 45,
        68, 248, 23, 46, 6, 140, 247, 18, 52, 37, 9, 32, 38, 102, 12, 190, 8, 212, 237, 12, 6, 66,
        65, 66, 69, 181, 1, 1, 0, 0, 0, 0, 253, 121, 18, 16, 0, 0, 0, 0, 182, 14, 80, 77, 46, 39,
        209, 60, 81, 14, 141, 206, 160, 50, 106, 233, 35, 123, 4, 185, 66, 182, 193, 156, 19, 45,
        137, 155, 123, 186, 11, 120, 251, 123, 81, 117, 113, 108, 169, 115, 142, 208, 243, 50, 102,
        4, 117, 254, 247, 226, 199, 113, 132, 25, 141, 90, 247, 19, 211, 5, 152, 96, 121, 6, 40,
        217, 92, 0, 33, 38, 199, 73, 36, 129, 161, 159, 184, 208, 215, 110, 150, 127, 221, 158, 50,
        102, 118, 40, 146, 24, 8, 98, 7, 56, 144, 0, 4, 66, 69, 69, 70, 132, 3, 39, 11, 33, 224,
        56, 100, 17, 18, 118, 159, 167, 103, 10, 86, 125, 222, 20, 189, 120, 236, 48, 202, 89, 180,
        71, 31, 56, 185, 23, 33, 23, 87, 5, 66, 65, 66, 69, 1, 1, 180, 253, 231, 90, 196, 206, 208,
        183, 14, 97, 124, 243, 43, 160, 133, 94, 19, 162, 126, 19, 7, 15, 222, 73, 114, 113, 104,
        78, 24, 52, 113, 47, 39, 154, 108, 148, 28, 146, 180, 232, 199, 20, 52, 170, 93, 214, 0,
        109, 168, 175, 162, 91, 234, 195, 228, 139, 236, 170, 251, 200, 178, 123, 26, 130,
    ])
    .unwrap();
}
