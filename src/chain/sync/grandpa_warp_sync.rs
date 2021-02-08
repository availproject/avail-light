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

use crate::{
    chain::chain_information::{
        babe_fetch_epoch, BabeEpochInformation, ChainInformation, ChainInformationConsensus,
        ChainInformationFinality,
    },
    executor::{
        host::{HostVmPrototype, NewErr},
        vm::ExecHint,
        DEFAULT_HEAP_PAGES,
    },
    finality::{grandpa::warp_sync, justification::verify},
    header::{Header, HeaderRef},
    network::protocol::GrandpaWarpSyncResponseFragment,
};
use core::convert::TryInto as _;

/// Problem encountered during a call to [`grandpa_warp_sync`].
#[derive(Debug, derive_more::Display)]
pub enum Error {
    #[display(fmt = "Missing :code")]
    MissingCode,
    #[display(fmt = "Failed to parse heap pages: {}", _0)]
    FailedToParseHeapPages(std::array::TryFromSliceError),
    #[display(fmt = "{}", _0)]
    Verifier(verify::Error),
    #[display(fmt = "{}", _0)]
    BabeFetchEpoch(babe_fetch_epoch::Error),
    #[display(fmt = "{}", _0)]
    NewRuntime(NewErr),
}

/// The configuration for [`grandpa_warp_sync`].
pub struct Config {
    /// The chain information of the starting point of the warp syncing.
    pub start_chain_information: ChainInformation,
    /// The initial capacity of the list of sources.
    pub sources_capacity: usize,
}

/// Starts syncing via GrandPa warp sync.
pub fn grandpa_warp_sync<TSrc>(config: Config) -> GrandpaWarpSync<TSrc> {
    GrandpaWarpSync::WaitingForSources(WaitingForSources {
        start_chain_information: config.start_chain_information,
        sources: Vec::with_capacity(config.sources_capacity),
        previous_verifier_values: None,
    })
}

/// The GrandPa warp sync state machine.
pub enum GrandpaWarpSync<TSrc> {
    /// Warp syncing is over.
    Finished(Result<(ChainInformation, HostVmPrototype), Error>),
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet<TSrc>),
    /// Fetching the key that follows a given one is required in order to continue.
    NextKey(NextKey<TSrc>),
    /// Verifying the warp sync response is required to continue.
    Verifier(Verifier<TSrc>),
    /// Requesting GrandPa warp sync data from a source is required to continue.
    WarpSyncRequest(WarpSyncRequest<TSrc>),
    /// Fetching the parameters for the virtual machine is required to continue.
    VirtualMachineParamsGet(VirtualMachineParamsGet<TSrc>),
    /// Adding more sources of GrandPa warp sync data to is required to continue.
    WaitingForSources(WaitingForSources<TSrc>),
}

impl<TSrc> GrandpaWarpSync<TSrc> {
    fn from_babe_fetch_epoch_query(
        query: babe_fetch_epoch::Query,
        fetched_current_epoch: Option<BabeEpochInformation>,
        state: PostVerificationState<TSrc>,
    ) -> Self {
        match (query, fetched_current_epoch) {
            (babe_fetch_epoch::Query::Finished(Ok((next_epoch, runtime))), Some(current_epoch)) => {
                let slots_per_epoch = match state.start_chain_information.consensus {
                    ChainInformationConsensus::Babe {
                        slots_per_epoch, ..
                    } => slots_per_epoch,
                    _ => unreachable!(),
                };

                Self::Finished(Ok((
                    ChainInformation {
                        finalized_block_header: state.header,
                        finality: state.chain_information_finality,
                        consensus: ChainInformationConsensus::Babe {
                            finalized_block_epoch_information: Some(current_epoch),
                            finalized_next_epoch_transition: next_epoch,
                            slots_per_epoch,
                        },
                    },
                    runtime,
                )))
            }
            (babe_fetch_epoch::Query::Finished(Ok((current_epoch, runtime))), None) => {
                let babe_next_epoch_query =
                    babe_fetch_epoch::babe_fetch_epoch(babe_fetch_epoch::Config {
                        runtime,
                        epoch_to_fetch: babe_fetch_epoch::BabeEpochToFetch::NextEpoch,
                    });
                Self::from_babe_fetch_epoch_query(babe_next_epoch_query, Some(current_epoch), state)
            }
            (babe_fetch_epoch::Query::Finished(Err(error)), _) => {
                Self::Finished(Err(Error::BabeFetchEpoch(error)))
            }
            (babe_fetch_epoch::Query::StorageGet(storage_get), fetched_current_epoch) => {
                Self::StorageGet(StorageGet {
                    inner: storage_get,
                    fetched_current_epoch,
                    state,
                })
            }
            (babe_fetch_epoch::Query::NextKey(next_key), fetched_current_epoch) => {
                Self::NextKey(NextKey {
                    inner: next_key,
                    fetched_current_epoch,
                    state,
                })
            }
        }
    }
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet<TSrc> {
    inner: babe_fetch_epoch::StorageGet,
    fetched_current_epoch: Option<BabeEpochInformation>,
    state: PostVerificationState<TSrc>,
}

impl<TSrc> StorageGet<TSrc> {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    pub fn key<'a>(&'a self) -> impl Iterator<Item = impl AsRef<[u8]> + 'a> + 'a {
        self.inner.key()
    }

    /// Returns the source that we received the warp sync data from.
    pub fn warp_sync_source(&self) -> &TSrc {
        &self.state.warp_sync_source
    }

    /// Returns the header that we're warp syncing up to.
    pub fn warp_sync_header(&self) -> HeaderRef {
        (&self.state.header).into()
    }

    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    ///
    /// This method is a shortcut for calling `key` and concatenating the returned slices.
    pub fn key_as_vec(&self) -> Vec<u8> {
        self.inner.key_as_vec()
    }

    /// Injects the corresponding storage value.
    pub fn inject_value(
        self,
        value: Option<impl Iterator<Item = impl AsRef<[u8]>>>,
    ) -> GrandpaWarpSync<TSrc> {
        GrandpaWarpSync::from_babe_fetch_epoch_query(
            self.inner.inject_value(value),
            self.fetched_current_epoch,
            self.state,
        )
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct NextKey<TSrc> {
    inner: babe_fetch_epoch::NextKey,
    fetched_current_epoch: Option<BabeEpochInformation>,
    state: PostVerificationState<TSrc>,
}

impl<TSrc> NextKey<TSrc> {
    /// Returns the key whose next key must be passed back.
    pub fn key<'a>(&'a self) -> impl AsRef<[u8]> + 'a {
        self.inner.key()
    }

    /// Returns the source that we received the warp sync data from.
    pub fn warp_sync_source(&self) -> &TSrc {
        &self.state.warp_sync_source
    }

    /// Returns the header that we're warp syncing up to.
    pub fn warp_sync_header(&self) -> HeaderRef {
        (&self.state.header).into()
    }

    /// Injects the key.
    ///
    /// # Panic
    ///
    /// Panics if the key passed as parameter isn't strictly superior to the requested key.
    ///
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> GrandpaWarpSync<TSrc> {
        GrandpaWarpSync::from_babe_fetch_epoch_query(
            self.inner.inject_key(key),
            self.fetched_current_epoch,
            self.state,
        )
    }
}

/// Verifying the warp sync response is required to continue.
pub struct Verifier<TSrc> {
    verifier: warp_sync::Verifier,
    start_chain_information: ChainInformation,
    warp_sync_source_index: usize,
    sources: Vec<TSrc>,
    final_set_of_fragments: bool,
}

impl<TSrc> Verifier<TSrc> {
    pub fn next(mut self) -> GrandpaWarpSync<TSrc> {
        match self.verifier.next() {
            Ok(warp_sync::Next::NotFinished(next_verifier)) => GrandpaWarpSync::Verifier(Self {
                verifier: next_verifier,
                start_chain_information: self.start_chain_information,
                sources: self.sources,
                warp_sync_source_index: self.warp_sync_source_index,
                final_set_of_fragments: self.final_set_of_fragments,
            }),
            Ok(warp_sync::Next::Success {
                header,
                chain_information_finality,
            }) => {
                if self.final_set_of_fragments {
                    GrandpaWarpSync::VirtualMachineParamsGet(VirtualMachineParamsGet {
                        state: PostVerificationState {
                            header,
                            chain_information_finality,
                            start_chain_information: self.start_chain_information,
                            warp_sync_source: self.sources.remove(self.warp_sync_source_index),
                        },
                    })
                } else {
                    GrandpaWarpSync::WarpSyncRequest(WarpSyncRequest {
                        source_index: self.warp_sync_source_index,
                        sources: self.sources,
                        start_chain_information: self.start_chain_information,
                        previous_verifier_values: Some((header, chain_information_finality)),
                    })
                }
            }
            Err(error) => GrandpaWarpSync::Finished(Err(Error::Verifier(error))),
        }
    }
}

struct PostVerificationState<TSrc> {
    header: Header,
    chain_information_finality: ChainInformationFinality,
    start_chain_information: ChainInformation,
    warp_sync_source: TSrc,
}

/// Requesting GrandPa warp sync data from a source is required to continue.
pub struct WarpSyncRequest<TSrc> {
    source_index: usize,
    sources: Vec<TSrc>,
    start_chain_information: ChainInformation,
    previous_verifier_values: Option<(Header, ChainInformationFinality)>,
}

impl<TSrc: PartialEq> WarpSyncRequest<TSrc> {
    /// The source to make a GrandPa warp sync request to.
    pub fn current_source(&self) -> &TSrc {
        &self.sources[self.source_index]
    }

    /// The hash of the header to warp sync from.
    pub fn start_block_hash(&self) -> [u8; 32] {
        match self.previous_verifier_values.as_ref() {
            Some((header, _)) => header.hash(),
            None => self.start_chain_information.finalized_block_header.hash(),
        }
    }

    /// Add a source to the list of sources.
    pub fn add_source(&mut self, source: TSrc) {
        assert!(!self.sources.iter().any(|s| s == &source));
        self.sources.push(source);
    }

    /// Remove a source from the list of sources.
    ///
    /// # Panic
    ///
    /// Panics if the source wasn't added to the list earlier.
    ///
    pub fn remove_source(mut self, to_remove: TSrc) -> GrandpaWarpSync<TSrc> {
        if &to_remove == self.current_source() {
            let next_index = self.source_index + 1;

            if next_index == self.sources.len() {
                GrandpaWarpSync::WaitingForSources(WaitingForSources {
                    sources: self.sources,
                    start_chain_information: self.start_chain_information,
                    previous_verifier_values: self.previous_verifier_values,
                })
            } else {
                GrandpaWarpSync::WarpSyncRequest(Self {
                    source_index: next_index,
                    sources: self.sources,
                    start_chain_information: self.start_chain_information,
                    previous_verifier_values: self.previous_verifier_values,
                })
            }
        } else {
            let index = self
                .sources
                .iter()
                .position(|source| source == &to_remove)
                .unwrap();

            self.sources.remove(index);

            if index < self.source_index {
                self.source_index -= 1;
            }

            GrandpaWarpSync::WarpSyncRequest(self)
        }
    }

    /// Submit a GrandPa warp sync response if the request succeeded or `None` if it did not.
    pub fn handle_response(
        mut self,
        mut response: Option<Vec<GrandpaWarpSyncResponseFragment>>,
    ) -> GrandpaWarpSync<TSrc> {
        // Count a response of 0 fragments as a failed response.
        if response
            .as_ref()
            .map(|fragments| fragments.is_empty())
            .unwrap_or(false)
        {
            response = None;
        }

        let next_index = self.source_index + 1;

        match response {
            Some(response_fragments) => {
                let final_set_of_fragments = response_fragments.len() == 1;

                let verifier = match self.previous_verifier_values {
                    Some((_, chain_information_finality)) => warp_sync::Verifier::new(
                        (&chain_information_finality).into(),
                        response_fragments,
                    ),
                    None => warp_sync::Verifier::new(
                        (&self.start_chain_information.finality).into(),
                        response_fragments,
                    ),
                };

                GrandpaWarpSync::Verifier(Verifier {
                    final_set_of_fragments,
                    verifier,
                    start_chain_information: self.start_chain_information,
                    sources: self.sources,
                    warp_sync_source_index: self.source_index,
                })
            }
            None if next_index < self.sources.len() => GrandpaWarpSync::WarpSyncRequest(Self {
                source_index: next_index,
                sources: self.sources,
                start_chain_information: self.start_chain_information,
                previous_verifier_values: self.previous_verifier_values,
            }),
            None => GrandpaWarpSync::WaitingForSources(WaitingForSources {
                sources: self.sources,
                start_chain_information: self.start_chain_information,
                previous_verifier_values: self.previous_verifier_values,
            }),
        }
    }
}

/// Fetching the parameters for the virtual machine is required to continue.
pub struct VirtualMachineParamsGet<TSrc> {
    state: PostVerificationState<TSrc>,
}

impl<TSrc> VirtualMachineParamsGet<TSrc> {
    /// Returns the header that we're warp syncing up to.
    pub fn warp_sync_header(&self) -> HeaderRef {
        (&self.state.header).into()
    }

    /// Set the code and heappages from storage using the keys `:code` and `:heappages`
    /// respectively. Also allows setting an execution hint for the virtual machine.
    pub fn set_virtual_machine_params(
        self,
        code: Option<impl AsRef<[u8]>>,
        heap_pages: Option<impl AsRef<[u8]>>,
        exec_hint: ExecHint,
    ) -> GrandpaWarpSync<TSrc> {
        let code = match code {
            Some(code) => code,
            None => return GrandpaWarpSync::Finished(Err(Error::MissingCode)),
        };

        let heap_pages = match heap_pages {
            Some(heap_pages) => match heap_pages.as_ref().try_into() {
                Ok(heap_pages) => u64::from_le_bytes(heap_pages),
                Err(error) => {
                    return GrandpaWarpSync::Finished(Err(Error::FailedToParseHeapPages(error)))
                }
            },
            None => DEFAULT_HEAP_PAGES,
        };

        match HostVmPrototype::new(code, heap_pages, exec_hint) {
            Ok(runtime) => {
                let babe_current_epoch_query =
                    babe_fetch_epoch::babe_fetch_epoch(babe_fetch_epoch::Config {
                        runtime,
                        epoch_to_fetch: babe_fetch_epoch::BabeEpochToFetch::CurrentEpoch,
                    });

                GrandpaWarpSync::from_babe_fetch_epoch_query(
                    babe_current_epoch_query,
                    None,
                    self.state,
                )
            }
            Err(error) => GrandpaWarpSync::Finished(Err(Error::NewRuntime(error))),
        }
    }
}

/// Adding more sources of GrandPa warp sync data to is required to continue.
pub struct WaitingForSources<TSrc> {
    sources: Vec<TSrc>,
    start_chain_information: ChainInformation,
    previous_verifier_values: Option<(Header, ChainInformationFinality)>,
}

impl<TSrc: PartialEq> WaitingForSources<TSrc> {
    /// Add a source to the list of sources.
    pub fn add_source(mut self, source: TSrc) -> GrandpaWarpSync<TSrc> {
        assert!(!self.sources.iter().any(|p| p == &source));

        self.sources.push(source);

        GrandpaWarpSync::WarpSyncRequest(WarpSyncRequest {
            source_index: self.sources.len() - 1,
            sources: self.sources,
            start_chain_information: self.start_chain_information,
            previous_verifier_values: self.previous_verifier_values,
        })
    }
}
