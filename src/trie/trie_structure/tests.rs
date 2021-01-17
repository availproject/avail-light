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

use super::{Nibble, TrieStructure};

use rand::{
    distributions::{Distribution as _, Uniform},
    seq::SliceRandom as _,
};
use std::{collections::HashSet, convert::TryFrom as _};

#[test]
fn remove_turns_storage_into_branch() {
    let with_removal = {
        let mut trie = TrieStructure::new();

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
                Nibble::try_from(10).unwrap(),
                Nibble::try_from(11).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
                Nibble::try_from(12).unwrap(),
                Nibble::try_from(13).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_occupied()
        .unwrap()
        .into_storage()
        .unwrap()
        .remove();

        trie
    };

    let expected = {
        let mut trie = TrieStructure::new();

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
                Nibble::try_from(10).unwrap(),
                Nibble::try_from(11).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
                Nibble::try_from(12).unwrap(),
                Nibble::try_from(13).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie
    };

    assert!(with_removal.structure_equal(&expected));
}

#[test]
fn insert_in_between() {
    let order1 = {
        let mut trie = TrieStructure::new();

        trie.node([Nibble::try_from(1).unwrap()].iter().cloned())
            .into_vacant()
            .unwrap()
            .insert_storage_value()
            .insert((), ());

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
                Nibble::try_from(4).unwrap(),
                Nibble::try_from(5).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie
    };

    let order2 = {
        let mut trie = TrieStructure::new();

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
                Nibble::try_from(4).unwrap(),
                Nibble::try_from(5).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie.node([Nibble::try_from(1).unwrap()].iter().cloned())
            .into_vacant()
            .unwrap()
            .insert_storage_value()
            .insert((), ());

        trie
    };

    let order3 = {
        let mut trie = TrieStructure::new();

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
                Nibble::try_from(4).unwrap(),
                Nibble::try_from(5).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie.node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

        trie.node([Nibble::try_from(1).unwrap()].iter().cloned())
            .into_vacant()
            .unwrap()
            .insert_storage_value()
            .insert((), ());

        trie
    };

    assert!(order1.structure_equal(&order2));
    assert!(order2.structure_equal(&order3));
    assert!(order1.structure_equal(&order3));
}

#[test]
fn insert_branch() {
    let mut trie = TrieStructure::new();

    trie.node([Nibble::try_from(1).unwrap()].iter().cloned())
        .into_vacant()
        .unwrap()
        .insert_storage_value()
        .insert((), ());

    trie.node(
        [
            Nibble::try_from(1).unwrap(),
            Nibble::try_from(2).unwrap(),
            Nibble::try_from(3).unwrap(),
            Nibble::try_from(4).unwrap(),
            Nibble::try_from(5).unwrap(),
        ]
        .iter()
        .cloned(),
    )
    .into_vacant()
    .unwrap()
    .insert_storage_value()
    .insert((), ());

    trie.node(
        [
            Nibble::try_from(1).unwrap(),
            Nibble::try_from(2).unwrap(),
            Nibble::try_from(3).unwrap(),
            Nibble::try_from(5).unwrap(),
            Nibble::try_from(6).unwrap(),
        ]
        .iter()
        .cloned(),
    )
    .into_vacant()
    .unwrap()
    .insert_storage_value()
    .insert((), ());

    assert!(!trie
        .node(
            [
                Nibble::try_from(1).unwrap(),
                Nibble::try_from(2).unwrap(),
                Nibble::try_from(3).unwrap(),
            ]
            .iter()
            .cloned(),
        )
        .into_occupied()
        .unwrap()
        .has_storage_value());
}

#[test]
fn fuzzing() {
    fn uniform_sample(min: u8, max: u8) -> u8 {
        Uniform::new_inclusive(min, max).sample(&mut rand::thread_rng())
    }

    // We run the test a couple times because of randomness.
    for _ in 0..16 {
        // Generate a set of keys that will find themselves in the tries in the end.
        let final_storage: HashSet<Vec<Nibble>> = {
            let mut list = vec![Vec::new()];
            for _ in 0..5 {
                for elem in list.clone().into_iter() {
                    for _ in 0..uniform_sample(0, 4) {
                        let mut elem = elem.clone();
                        for _ in 0..uniform_sample(0, 3) {
                            elem.push(Nibble::try_from(uniform_sample(0, 15)).unwrap());
                        }
                        list.push(elem);
                    }
                }
            }
            list.into_iter().skip(1).collect()
        };

        // Create multiple tries, each with a different order of insertion for the nodes.
        let mut tries = Vec::new();
        for _ in 0..16 {
            let mut operations = final_storage
                .iter()
                .map(|k| (k.clone(), true))
                .collect::<Vec<_>>();
            operations.shuffle(&mut rand::thread_rng());

            // Insert in `operations` a tuple of an insertion and removal.
            for _ in 0..uniform_sample(0, 24) {
                let mut base_key = match operations.choose(&mut rand::thread_rng()) {
                    Some(op) => op.0.clone(),
                    None => continue,
                };

                for _ in 0..uniform_sample(0, 2) {
                    base_key.push(Nibble::try_from(uniform_sample(0, 15)).unwrap());
                }

                let max_remove_index = operations
                    .iter()
                    .position(|(k, _)| *k == base_key)
                    .unwrap_or(operations.len());

                let remove_index =
                    Uniform::new_inclusive(0, max_remove_index).sample(&mut rand::thread_rng());
                let insert_index =
                    Uniform::new_inclusive(0, remove_index).sample(&mut rand::thread_rng());
                operations.insert(remove_index, (base_key.clone(), false));
                operations.insert(insert_index, (base_key, true));
            }

            // Create a trie and applies `operations` on it.
            let mut trie = TrieStructure::new();
            for (key, insert) in operations {
                if insert {
                    match trie.node(key.into_iter()) {
                        super::Entry::Vacant(e) => {
                            e.insert_storage_value().insert((), ());
                        }
                        super::Entry::Occupied(super::NodeAccess::Branch(e)) => {
                            e.insert_storage_value();
                        }
                        super::Entry::Occupied(super::NodeAccess::Storage(_)) => unreachable!(),
                    }
                } else {
                    match trie.node(key.into_iter()) {
                        super::Entry::Occupied(super::NodeAccess::Storage(e)) => {
                            e.remove();
                        }
                        super::Entry::Vacant(_) => unreachable!(),
                        super::Entry::Occupied(super::NodeAccess::Branch(_)) => unreachable!(),
                    }
                }
            }

            tries.push(trie);
        }

        // Compare them to make sure they're equal.
        for trie in 1..tries.len() {
            tries[0].structure_equal(&tries[trie]);
        }
    }
}
