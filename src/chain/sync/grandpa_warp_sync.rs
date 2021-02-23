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
        babe_fetch_epoch::{self, PartialBabeEpochInformation},
        BabeEpochInformation, ChainInformation, ChainInformationConsensus,
        ChainInformationFinality, ChainInformationRef,
    },
    executor::{
        self,
        host::{HostVmPrototype, NewErr},
        vm::ExecHint,
    },
    finality::{grandpa::warp_sync, justification::verify},
    header::{Header, HeaderRef},
    network::protocol::GrandpaWarpSyncResponseFragment,
};

/// Problem encountered during a call to [`grandpa_warp_sync`].
#[derive(Debug, derive_more::Display)]
pub enum Error {
    #[display(fmt = "Missing :code")]
    MissingCode,
    #[display(fmt = "{}", _0)]
    InvalidHeapPages(executor::InvalidHeapPagesError),
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
        state: PreVerificationState {
            start_chain_information: config.start_chain_information,
        },
        sources: slab::Slab::with_capacity(config.sources_capacity),
        previous_verifier_values: None,
    })
}

/// Identifier for a source in the [`GrandpaWarpSync`].
//
// Implementation note: this represents the index within the `Slab` used for the list of sources.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct SourceId(usize);

/// The GrandPa warp sync state machine.
#[derive(derive_more::From)]
pub enum GrandpaWarpSync<TSrc> {
    /// Warp syncing is over.
    Finished(Result<(ChainInformation, HostVmPrototype), Error>),
    /// Loading a storage value is required in order to continue.
    #[from]
    StorageGet(StorageGet<TSrc>),
    /// Fetching the key that follows a given one is required in order to continue.
    #[from]
    NextKey(NextKey<TSrc>),
    /// Verifying the warp sync response is required to continue.
    #[from]
    Verifier(Verifier<TSrc>),
    /// Requesting GrandPa warp sync data from a source is required to continue.
    #[from]
    WarpSyncRequest(WarpSyncRequest<TSrc>),
    /// Fetching the parameters for the virtual machine is required to continue.
    #[from]
    VirtualMachineParamsGet(VirtualMachineParamsGet<TSrc>),
    /// Adding more sources of GrandPa warp sync data to is required to continue.
    #[from]
    WaitingForSources(WaitingForSources<TSrc>),
}

impl<TSrc> GrandpaWarpSync<TSrc> {
    fn from_babe_fetch_epoch_query(
        query: babe_fetch_epoch::Query,
        fetched_current_epoch: Option<PartialBabeEpochInformation>,
        state: PostVerificationState<TSrc>,
    ) -> Self {
        match (query, fetched_current_epoch) {
            (babe_fetch_epoch::Query::Finished(Ok((next_epoch, runtime))), Some(current_epoch)) => {
                let (slots_per_epoch, babe_config_c, babe_config_allowed_slots) =
                    match state.start_chain_information.consensus {
                        ChainInformationConsensus::Babe {
                            slots_per_epoch,
                            finalized_next_epoch_transition,
                            ..
                        } => (
                            slots_per_epoch,
                            // TODO: /!\ /!\ shouldn't take the same configuration as the genesis; this is a hack while waiting for https://github.com/paritytech/substrate/issues/8060
                            finalized_next_epoch_transition.c,
                            finalized_next_epoch_transition.allowed_slots,
                        ),
                        _ => unreachable!(),
                    };

                Self::Finished(Ok((
                    ChainInformation {
                        finalized_block_header: state.header,
                        finality: state.chain_information_finality,
                        consensus: ChainInformationConsensus::Babe {
                            finalized_block_epoch_information: Some(BabeEpochInformation {
                                epoch_index: current_epoch.epoch_index,
                                start_slot_number: current_epoch.start_slot_number,
                                authorities: current_epoch.authorities,
                                randomness: current_epoch.randomness,
                                c: babe_config_c,
                                allowed_slots: babe_config_allowed_slots,
                            }),
                            finalized_next_epoch_transition: BabeEpochInformation {
                                epoch_index: next_epoch.epoch_index,
                                start_slot_number: next_epoch.start_slot_number,
                                authorities: next_epoch.authorities,
                                randomness: next_epoch.randomness,
                                c: babe_config_c,
                                allowed_slots: babe_config_allowed_slots,
                            },
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
    fetched_current_epoch: Option<PartialBabeEpochInformation>,
    state: PostVerificationState<TSrc>,
}

impl<TSrc> StorageGet<TSrc> {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    pub fn key(&'_ self) -> impl Iterator<Item = impl AsRef<[u8]> + '_> + '_ {
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

    /// Returns the chain information that is considered fully verified.
    pub fn as_chain_information(&self) -> ChainInformationRef {
        (&self.state.start_chain_information).into()
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
    fetched_current_epoch: Option<PartialBabeEpochInformation>,
    state: PostVerificationState<TSrc>,
}

impl<TSrc> NextKey<TSrc> {
    /// Returns the key whose next key must be passed back.
    pub fn key(&'_ self) -> impl AsRef<[u8]> + '_ {
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

    /// Returns the chain information that is considered fully verified.
    pub fn as_chain_information(&self) -> ChainInformationRef {
        (&self.state.start_chain_information).into()
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
    state: PreVerificationState,
    warp_sync_source_id: SourceId,
    sources: slab::Slab<Source<TSrc>>,
    final_set_of_fragments: bool,
}

impl<TSrc> Verifier<TSrc> {
    /// Returns the chain information that is considered verified.
    pub fn as_chain_information(&self) -> ChainInformationRef {
        (&self.state.start_chain_information).into()
    }

    pub fn next(mut self) -> GrandpaWarpSync<TSrc> {
        match self.verifier.next() {
            Ok(warp_sync::Next::NotFinished(next_verifier)) => GrandpaWarpSync::Verifier(Self {
                verifier: next_verifier,
                state: self.state,
                sources: self.sources,
                warp_sync_source_id: self.warp_sync_source_id,
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
                            start_chain_information: self.state.start_chain_information,
                            warp_sync_source: self
                                .sources
                                .remove(self.warp_sync_source_id.0)
                                .user_data,
                        },
                    })
                } else {
                    GrandpaWarpSync::WarpSyncRequest(WarpSyncRequest {
                        source_id: self.warp_sync_source_id,
                        sources: self.sources,
                        state: self.state,
                        previous_verifier_values: Some((header, chain_information_finality)),
                    })
                }
            }
            Err(error) => GrandpaWarpSync::Finished(Err(Error::Verifier(error))),
        }
    }
}

struct PreVerificationState {
    start_chain_information: ChainInformation,
}

struct PostVerificationState<TSrc> {
    header: Header,
    chain_information_finality: ChainInformationFinality,
    start_chain_information: ChainInformation,
    warp_sync_source: TSrc,
}

/// Requesting GrandPa warp sync data from a source is required to continue.
pub struct WarpSyncRequest<TSrc> {
    source_id: SourceId,
    sources: slab::Slab<Source<TSrc>>,
    state: PreVerificationState,
    previous_verifier_values: Option<(Header, ChainInformationFinality)>,
}

impl<TSrc> WarpSyncRequest<TSrc> {
    /// The source to make a GrandPa warp sync request to.
    pub fn current_source(&self) -> (SourceId, &TSrc) {
        (self.source_id, &self.sources[self.source_id.0].user_data)
    }

    /// The hash of the header to warp sync from.
    pub fn start_block_hash(&self) -> [u8; 32] {
        match self.previous_verifier_values.as_ref() {
            Some((header, _)) => header.hash(),
            None => self
                .state
                .start_chain_information
                .finalized_block_header
                .hash(),
        }
    }

    /// Returns the chain information that is considered verified.
    pub fn as_chain_information(&self) -> ChainInformationRef {
        (&self.state.start_chain_information).into()
    }

    /// Returns the user data (`TSrc`) corresponding to the given source.
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is invalid.
    ///
    pub fn source_user_data_mut(&mut self, source_id: SourceId) -> &mut TSrc {
        debug_assert!(self.sources.contains(source_id.0));
        &mut self.sources[source_id.0].user_data
    }

    /// Add a source to the list of sources.
    pub fn add_source(&mut self, user_data: TSrc) -> SourceId {
        SourceId(self.sources.insert(Source {
            user_data,
            already_tried: false,
        }))
    }

    /// Remove a source from the list of sources.
    ///
    /// # Panic
    ///
    /// Panics if the source wasn't added to the list earlier.
    ///
    pub fn remove_source(mut self, to_remove: SourceId) -> (TSrc, GrandpaWarpSync<TSrc>) {
        if to_remove == self.source_id {
            let next_id = self
                .sources
                .iter()
                .find(|(_, s)| !s.already_tried)
                .map(|(id, _)| SourceId(id));

            let removed = self.sources.remove(to_remove.0).user_data;

            let next_state = if let Some(next_id) = next_id {
                GrandpaWarpSync::WarpSyncRequest(Self {
                    source_id: next_id,
                    sources: self.sources,
                    state: self.state,
                    previous_verifier_values: self.previous_verifier_values,
                })
            } else {
                GrandpaWarpSync::WaitingForSources(WaitingForSources {
                    sources: self.sources,
                    state: self.state,
                    previous_verifier_values: self.previous_verifier_values,
                })
            };

            (removed, next_state)
        } else {
            let removed = self.sources.remove(to_remove.0).user_data;
            (removed, GrandpaWarpSync::WarpSyncRequest(self))
        }
    }

    /// Submit a GrandPa warp sync response if the request succeeded or `None` if it did not.
    pub fn handle_response(
        mut self,
        mut response: Option<Vec<GrandpaWarpSyncResponseFragment>>,
    ) -> GrandpaWarpSync<TSrc> {
        self.sources[self.source_id.0].already_tried = true;

        // Count a response of 0 fragments as a failed response.
        if response
            .as_ref()
            .map(|fragments| fragments.is_empty())
            .unwrap_or(false)
        {
            response = None;
        }

        match response {
            Some(response_fragments) => {
                let final_set_of_fragments = response_fragments.len() == 1;

                let verifier = match self.previous_verifier_values {
                    Some((_, chain_information_finality)) => warp_sync::Verifier::new(
                        (&chain_information_finality).into(),
                        response_fragments,
                    ),
                    None => warp_sync::Verifier::new(
                        (&self.state.start_chain_information.finality).into(),
                        response_fragments,
                    ),
                };

                GrandpaWarpSync::Verifier(Verifier {
                    final_set_of_fragments,
                    verifier,
                    state: self.state,
                    sources: self.sources,
                    warp_sync_source_id: self.source_id,
                })
            }
            None => {
                let next_id = self
                    .sources
                    .iter()
                    .find(|(_, s)| !s.already_tried)
                    .map(|(id, _)| SourceId(id));

                if let Some(next_id) = next_id {
                    GrandpaWarpSync::WarpSyncRequest(Self {
                        source_id: next_id,
                        sources: self.sources,
                        state: self.state,
                        previous_verifier_values: self.previous_verifier_values,
                    })
                } else {
                    GrandpaWarpSync::WaitingForSources(WaitingForSources {
                        sources: self.sources,
                        state: self.state,
                        previous_verifier_values: self.previous_verifier_values,
                    })
                }
            }
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

    /// Returns the chain information that is considered fully verified.
    pub fn as_chain_information(&self) -> ChainInformationRef {
        (&self.state.start_chain_information).into()
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

        let heap_pages =
            match executor::storage_heap_pages_to_value(heap_pages.as_ref().map(|p| p.as_ref())) {
                Ok(hp) => hp,
                Err(err) => return GrandpaWarpSync::Finished(Err(Error::InvalidHeapPages(err))),
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
    /// List of sources. It is guaranteed that they all have `already_tried` equal to `true`.
    sources: slab::Slab<Source<TSrc>>,
    state: PreVerificationState,
    previous_verifier_values: Option<(Header, ChainInformationFinality)>,
}

impl<TSrc> WaitingForSources<TSrc> {
    /// Add a source to the list of sources.
    pub fn add_source(mut self, user_data: TSrc) -> WarpSyncRequest<TSrc> {
        let source_id = SourceId(self.sources.insert(Source {
            user_data,
            already_tried: false,
        }));

        WarpSyncRequest {
            source_id,
            sources: self.sources,
            state: self.state,
            previous_verifier_values: self.previous_verifier_values,
        }
    }

    /// Returns the user data (`TSrc`) corresponding to the given source.
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is invalid.
    ///
    pub fn source_user_data_mut(&mut self, source_id: SourceId) -> &mut TSrc {
        debug_assert!(self.sources.contains(source_id.0));
        &mut self.sources[source_id.0].user_data
    }

    /// Returns the chain information that is considered fully verified.
    pub fn as_chain_information(&self) -> ChainInformationRef {
        (&self.state.start_chain_information).into()
    }
}

#[derive(Debug, Copy, Clone)]
struct Source<TSrc> {
    user_data: TSrc,
    /// `true` if this source has been in a past `WarpSyncRequest`. `false` if the source is
    /// currently in a `WarpSyncRequest`.
    already_tried: bool,
}
