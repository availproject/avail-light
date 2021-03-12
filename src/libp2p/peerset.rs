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

//! Data structure storing a networking state. Helper for managing connectivity to overlay
//! networks.
//!
//! The [`Peerset`] is a data structure that holds a list of node identities ([`PeerId`]s) and a
//! list of overlay networks. Each [`PeerId`] is associated with:
//!
//! - A list of active inbound connections.
//! - A list of [`Multiaddr`]s onto which the node is believed to be reachable.
//!   - For each multiaddr, optionally an active connection or pending dialing attempt.
//! - A list of overlay networks the node is believed to belong to.
//!   - For each overlay network the node belongs to, one optional inbound and one optional
//!     outbound substream.
//!     - Each substream can be either "pending" or "established".
//!
//! > **Note**: The [`Peerset`] does not *do* anything by itself, such as opening new connections.
//! >           it is purely a data structure that helps organize and maintain information about
//! >           the network.
//!
//! # Usage
//!
//! The [`Peerset`] must be initialized with a list of overlay networks the node is interested in.
//!
//! It is assumed that some discovery mechanism, not covered by this module, is in place in order
//! to discover the identities and addresses of nodes that belong to these various overlay
//! networks.
//!

// TODO: finish documentation

use crate::libp2p::peer_id::PeerId;

use ahash::RandomState;
use alloc::{
    collections::{btree_map, BTreeMap, BTreeSet},
    vec::Vec,
};
use core::mem;
use hashbrown::HashMap;
use parity_multiaddr::Multiaddr;
use rand::{seq::IteratorRandom as _, RngCore as _, SeedableRng as _};

/// Configuration for a [`Peerset`].
#[derive(Debug)]
pub struct Config {
    /// Capacity to reserve for containers having a number of peers.
    pub peers_capacity: usize,

    /// Number of overlay networks managed by the [`Peerset`]. The overlay networks are numbered
    /// from 0 to this value excluded.
    pub num_overlay_networks: usize,

    /// Seed for the randomness used to decide how peers are chosen.
    pub randomness_seed: [u8; 32],
}

/// See the [module-level documentation](self).
pub struct Peerset<TPeer, TConn, TPending, TSub, TPendingSub> {
    /// Same as [`Config::num_overlay_networks`].
    num_overlay_networks: usize,

    /// For every known [`PeerId`], its index in [`Peerset::peers`].
    peer_ids: HashMap<PeerId, usize, RandomState>,

    /// List of all known peers and their data.
    peers: slab::Slab<Peer<TPeer>>,

    /// Active and pending connections.
    connections: slab::Slab<Connection<TConn, TPending>>,

    /// Incremented by one every time a connection goes into a "connected" state. Decremented when
    /// a connection is closed.
    num_established_connections: usize,

    /// PRNG used to randomly select nodes.
    rng: rand_chacha::ChaCha20Rng,

    /// Container that holds tuples of `(peer_index, connection_index)`. Contains the combinations
    /// of connections associated to a certain peer.
    peer_connections: BTreeSet<(usize, usize)>,

    /// Container that holds tuples of `(overlay_index, peer_index)`.
    /// Contains combinations where the peer belongs to the overlay network.
    overlay_peers: BTreeSet<(usize, usize)>,

    /// Container that holds tuples of `(peer_index, overlay_index)`.
    /// Contains combinations where the peer belongs to the overlay network.
    peers_overlays: BTreeSet<(usize, usize)>,

    /// Container that holds tuples of `(connection_index, overlay_index, direction)`.
    connection_overlays:
        BTreeMap<(usize, usize, SubstreamDirection), SubstreamState<TSub, TPendingSub>>,
}

struct Peer<TPeer> {
    peer_id: PeerId,
    user_data: TPeer,
    addresses: Vec<Multiaddr>,
}

struct Connection<TConn, TPending> {
    peer_index: usize,
    ty: ConnectionTy<TConn, TPending>,
}

enum ConnectionTy<TConn, TPending> {
    Poisoned,
    Connected {
        user_data: TConn,
        inbound: bool,
    },
    Pending {
        user_data: TPending,
        target: Multiaddr,
    },
}

/// Direction of a substream.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub enum SubstreamDirection {
    /// Substream opened by the remote.
    In,
    /// Substream opened by the local node.
    Out,
}

/// > **Note**: There is no `Closed` variant, as this corresponds to a lack of entry in the
/// >           hashmap.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
enum SubstreamState<TSub, TPendingSub> {
    Pending(TPendingSub),
    Open(TSub),
    Poisoned,
}

impl<TPeer, TConn, TPending, TSub, TPendingSub> Peerset<TPeer, TConn, TPending, TSub, TPendingSub> {
    /// Creates a [`Peerset`] with the given configuration.
    pub fn new(config: Config) -> Self {
        let mut rng = rand_chacha::ChaCha20Rng::from_seed(config.randomness_seed);

        let peer_ids = {
            let k0 = rng.next_u64();
            let k1 = rng.next_u64();
            let k2 = rng.next_u64();
            let k3 = rng.next_u64();
            HashMap::with_capacity_and_hasher(
                config.peers_capacity,
                RandomState::with_seeds(k0, k1, k2, k3),
            )
        };

        Peerset {
            num_overlay_networks: config.num_overlay_networks,
            rng,
            peer_ids,
            peers: slab::Slab::with_capacity(config.peers_capacity),
            num_established_connections: 0,
            connections: slab::Slab::with_capacity(config.peers_capacity * 2), // TODO: correct capacity?
            peer_connections: BTreeSet::new(),
            overlay_peers: BTreeSet::new(),
            peers_overlays: BTreeSet::new(),
            connection_overlays: BTreeMap::new(),
        }
    }

    /// Returns the number of established connections in the peerset.
    pub fn num_established_connections(&self) -> usize {
        debug_assert_eq!(
            self.connections
                .iter()
                .filter(|(_, c)| matches!(c.ty, ConnectionTy::Connected { .. }))
                .count(),
            self.num_established_connections
        );

        self.num_established_connections
    }

    /// Returns the [`PeerId`]s of all active connections.
    ///
    /// Since multiple connections to the same [`PeerId`] can exist, the same [`PeerId`] can be
    /// yielded multiple times.
    pub fn connections_peer_ids(&self) -> impl Iterator<Item = (ConnectionId, &PeerId)> {
        self.connections
            .iter()
            .filter(|(_, c)| matches!(c.ty, ConnectionTy::Connected { .. }))
            .map(move |(id, c)| (ConnectionId(id), &self.peers[c.peer_index].peer_id))
    }

    /// Returns the number of overlay networks registered towards the peerset.
    pub fn num_overlay_networks(&self) -> usize {
        self.num_overlay_networks
    }

    /// Returns the list of nodes that belong to the given overlay network.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn overlay_network_nodes(
        &self,
        overlay_network_index: usize,
    ) -> impl Iterator<Item = &PeerId> {
        assert!(overlay_network_index < self.num_overlay_networks);
        self.overlay_peers
            .range((overlay_network_index, 0)..=(overlay_network_index, usize::max_value()))
            .map(move |(_, id)| &self.peers[*id].peer_id)
    }

    /// Returns a random node in the list of nodes that match the following criterias:
    ///
    /// - Peerset has at least one outbound connection towards this node.
    /// - None of the connections towards this node have an outbound substream with the given
    /// overlay network.
    ///
    pub fn random_connected_closed_node(
        &mut self,
        overlay_network_index: usize,
    ) -> Option<NodeMutKnown<TPeer, TConn, TPending, TSub, TPendingSub>> {
        let peer_connections = &self.peer_connections;
        let connection_overlays = &self.connection_overlays;
        let connections = &self.connections;

        let peer_index = self
            .overlay_peers
            .range((overlay_network_index, 0)..=(overlay_network_index, usize::max_value()))
            .map(|(_, index)| *index)
            .filter(move |peer_index| {
                let mut iter = peer_connections
                    .range((*peer_index, 0)..=(*peer_index, usize::max_value()))
                    .map(|(_, connec_id)| *connec_id);
                if iter
                    .clone()
                    .find(|connec_id| match connections[*connec_id].ty {
                        ConnectionTy::Connected { inbound, .. } => !inbound,
                        ConnectionTy::Pending { .. } => false,
                        ConnectionTy::Poisoned => unreachable!(),
                    })
                    .is_none()
                {
                    return false;
                }

                !iter.any(|connec_id| {
                    connection_overlays.contains_key(&(
                        connec_id,
                        overlay_network_index,
                        SubstreamDirection::Out,
                    ))
                })
            })
            .choose(&mut self.rng)?;

        Some(NodeMutKnown {
            peerset: self,
            peer_index,
        })
    }

    /// Returns a random node in the list of nodes that match the following criterias:
    ///
    /// - Peerset has no connection nor pending connection towards this node.
    /// - Node belongs to the given overlay network.
    /// - Node has at least one known address.
    ///
    /// Returns `None` if no such node is available.
    pub fn random_not_connected(
        &mut self,
        overlay_network_index: usize,
    ) -> Option<NodeMutKnown<TPeer, TConn, TPending, TSub, TPendingSub>> {
        let peers = &self.peers;
        let peer_connections = &self.peer_connections;

        let peer_index = self
            .overlay_peers
            .range((overlay_network_index, 0)..=(overlay_network_index, usize::max_value()))
            .map(|(_, index)| *index)
            .filter(move |peer_index| {
                if peer_connections
                    .range((*peer_index, 0)..=(*peer_index, usize::max_value()))
                    .next()
                    .is_some()
                {
                    return false;
                }

                if peers.get(*peer_index).unwrap().addresses.is_empty() {
                    return false;
                }

                // TODO: check pending too
                true
            })
            .choose(&mut self.rng)?;

        Some(NodeMutKnown {
            peerset: self,
            peer_index,
        })
    }

    /// Gives access to a pending connection within the [`Peerset`].
    pub fn pending_mut(
        &mut self,
        id: ConnectionId,
    ) -> Option<PendingMut<TPeer, TConn, TPending, TSub, TPendingSub>> {
        if self
            .connections
            .get(id.0)
            .map_or(false, |c| matches!(c.ty, ConnectionTy::Pending { .. }))
        {
            Some(PendingMut { peerset: self, id })
        } else {
            None
        }
    }

    /// Gives access to a connection within the [`Peerset`].
    pub fn connection_mut(
        &mut self,
        id: ConnectionId,
    ) -> Option<ConnectionMut<TPeer, TConn, TPending, TSub, TPendingSub>> {
        if self
            .connections
            .get(id.0)
            .map_or(false, |c| matches!(c.ty, ConnectionTy::Connected { .. }))
        {
            Some(ConnectionMut { peerset: self, id })
        } else {
            None
        }
    }

    /// Gives access to a connection within the [`Peerset`].
    pub fn pending_or_connection_mut(
        &mut self,
        id: ConnectionId,
    ) -> Option<PendingOrConnectionMut<TPeer, TConn, TPending, TSub, TPendingSub>> {
        if let Some(c) = self.connections.get(id.0) {
            match c.ty {
                ConnectionTy::Connected { .. } => {
                    Some(PendingOrConnectionMut::Connection(ConnectionMut {
                        peerset: self,
                        id,
                    }))
                }
                ConnectionTy::Pending { .. } => Some(PendingOrConnectionMut::Pending(PendingMut {
                    peerset: self,
                    id,
                })),
                ConnectionTy::Poisoned => unreachable!(),
            }
        } else {
            None
        }
    }

    /// Gives access to the state of the node with the given identity.
    pub fn node_mut(
        &mut self,
        peer_id: PeerId,
    ) -> NodeMut<TPeer, TConn, TPending, TSub, TPendingSub> {
        if let Some(peer_index) = self.peer_ids.get(&peer_id).cloned() {
            NodeMut::Known(NodeMutKnown {
                peerset: self,
                peer_index,
            })
        } else {
            NodeMut::Unknown(NodeMutUnknown {
                peerset: self,
                peer_id,
            })
        }
    }
}

/// Access to a connection in the [`Peerset`].
pub enum PendingOrConnectionMut<'a, TPeer, TConn, TPending, TSub, TPendingSub> {
    /// Connection is in the pending state.
    Pending(PendingMut<'a, TPeer, TConn, TPending, TSub, TPendingSub>),
    /// Connection is in the established state.
    Connection(ConnectionMut<'a, TPeer, TConn, TPending, TSub, TPendingSub>),
}

/// Identifier for a connection in a [`Peerset`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(usize);

/// Access to a connection in the [`Peerset`].
pub struct ConnectionMut<'a, TPeer, TConn, TPending, TSub, TPendingSub> {
    peerset: &'a mut Peerset<TPeer, TConn, TPending, TSub, TPendingSub>,
    id: ConnectionId,
}

impl<'a, TPeer, TConn, TPending, TSub, TPendingSub>
    ConnectionMut<'a, TPeer, TConn, TPending, TSub, TPendingSub>
{
    /// Returns the identifier of this connection.
    pub fn id(&self) -> ConnectionId {
        self.id
    }

    /// [`PeerId`] the connection is connected to.
    pub fn peer_id(&self) -> &PeerId {
        let index = self.peerset.connections[self.id.0].peer_index;
        &self.peerset.peers[index].peer_id
    }

    /// Returns true if the connection is inbound.
    pub fn is_inbound(&self) -> bool {
        match self.peerset.connections[self.id.0].ty {
            ConnectionTy::Connected { inbound, .. } => inbound,
            _ => unreachable!(),
        }
    }

    /// Adds a pending substream of the given overlay network and direction to the connection.
    ///
    /// Only one substream per combination of connection, overlay network, and direction is
    /// allowed at a time. An error is returned containing the user data passed as parameter if
    /// there already exists an entry.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn add_pending_substream(
        &mut self,
        overlay_network: usize,
        direction: SubstreamDirection,
        user_data: TPendingSub,
    ) -> Result<(), TPendingSub> {
        assert!(overlay_network < self.peerset.num_overlay_networks);

        match self
            .peerset
            .connection_overlays
            .entry((self.id.0, overlay_network, direction))
        {
            btree_map::Entry::Occupied(_) => Err(user_data),
            btree_map::Entry::Vacant(e) => {
                e.insert(SubstreamState::Pending(user_data));
                Ok(())
            }
        }
    }

    /// Turns a pending substream into an established substream.
    ///
    /// Returns an error if there is no pending substream with this overlay network and direction
    /// combination.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn confirm_substream(
        &mut self,
        overlay_network: usize,
        direction: SubstreamDirection,
        user_data: impl FnOnce(TPendingSub) -> TSub,
    ) -> Result<(), ()> {
        assert!(overlay_network < self.peerset.num_overlay_networks);

        let entry = self
            .peerset
            .connection_overlays
            .get_mut(&(self.id.0, overlay_network, direction))
            .ok_or(())?;
        if let SubstreamState::Pending(ud) = mem::replace(entry, SubstreamState::Poisoned) {
            *entry = SubstreamState::Open(user_data(ud));
            Ok(())
        } else {
            Err(())
        }
    }

    /// Returns the user data, if any, of a pending substream on this overlay network and this
    /// direction.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn pending_substream_user_data_mut(
        &mut self,
        overlay_network: usize,
        direction: SubstreamDirection,
    ) -> Option<&mut TPendingSub> {
        assert!(overlay_network < self.peerset.num_overlay_networks);
        match self
            .peerset
            .connection_overlays
            .get_mut(&(self.id.0, overlay_network, direction))
        {
            Some(SubstreamState::Pending(ud)) => Some(ud),
            _ => None,
        }
    }

    /// Removes a pending substream.
    ///
    /// Returns an error if there is no pending substream with this overlay network and direction
    /// combination.
    pub fn remove_pending_substream(
        &mut self,
        overlay_network: usize,
        direction: SubstreamDirection,
    ) -> Result<TPendingSub, ()> {
        assert!(overlay_network < self.peerset.num_overlay_networks);

        if let btree_map::Entry::Occupied(mut entry) =
            self.peerset
                .connection_overlays
                .entry((self.id.0, overlay_network, direction))
        {
            if matches!(entry.get_mut(), SubstreamState::Pending(_)) {
                match entry.remove() {
                    SubstreamState::Pending(ud) => Ok(ud),
                    _ => unreachable!(),
                }
            } else {
                Err(())
            }
        } else {
            Err(())
        }
    }

    /// Removes an established substream.
    ///
    /// Returns an error if there is no established substream with this overlay network and
    /// direction combination.
    pub fn remove_substream(
        &mut self,
        overlay_network: usize,
        direction: SubstreamDirection,
    ) -> Result<TSub, ()> {
        assert!(overlay_network < self.peerset.num_overlay_networks);

        if let btree_map::Entry::Occupied(mut entry) =
            self.peerset
                .connection_overlays
                .entry((self.id.0, overlay_network, direction))
        {
            if matches!(entry.get_mut(), SubstreamState::Open(_)) {
                match entry.remove() {
                    SubstreamState::Open(ud) => Ok(ud),
                    _ => unreachable!(),
                }
            } else {
                Err(())
            }
        } else {
            Err(())
        }
    }

    /// Returns `true` if there exists an open substream on this overlay network and this
    /// direction.
    ///
    /// This only returns `true` if the substream is open. If there exists a pending substream,
    /// `false` is returned.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn has_open_substream(
        &mut self,
        overlay_network: usize,
        direction: SubstreamDirection,
    ) -> bool {
        assert!(overlay_network < self.peerset.num_overlay_networks);
        match self
            .peerset
            .connection_overlays
            .get(&(self.id.0, overlay_network, direction))
        {
            Some(SubstreamState::Open(_)) => true,
            _ => false,
        }
    }

    /// Returns the list of open substreams of this connection.
    ///
    /// > **Note**: *Pending* substreams aren't returned.
    pub fn open_substreams_mut(
        &mut self,
    ) -> impl Iterator<Item = (usize, SubstreamDirection, &mut TSub)> {
        self.peerset
            .connection_overlays
            .range_mut(
                (self.id.0, 0, SubstreamDirection::In)
                    ..=(self.id.0, usize::max_value(), SubstreamDirection::Out),
            )
            .filter_map(
                move |(&(_, ref overlay_network_index, ref direction), &mut ref mut state)| {
                    match state {
                        SubstreamState::Open(ud) => Some((*overlay_network_index, *direction, ud)),
                        SubstreamState::Pending(_) => None,
                        SubstreamState::Poisoned => unreachable!(),
                    }
                },
            )
    }

    /// Gives access to the user data associated with the connection.
    pub fn user_data_mut(&mut self) -> &mut TConn {
        match &mut self.peerset.connections[self.id.0].ty {
            ConnectionTy::Connected { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }

    /// Gives access to the user data associated with the connection.
    pub fn into_user_data(self) -> &'a mut TConn {
        match &mut self.peerset.connections[self.id.0].ty {
            ConnectionTy::Connected { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }

    /// Removes the connection from the data structure.
    pub fn remove(self) -> TConn {
        let connection = self.peerset.connections.remove(self.id.0);
        let _was_in = self
            .peerset
            .peer_connections
            .remove(&(connection.peer_index, self.id.0));
        debug_assert!(_was_in);
        let overlays = self
            .peerset
            .connection_overlays
            .range_mut(
                (self.id.0, 0, SubstreamDirection::In)
                    ..=(self.id.0, usize::max_value(), SubstreamDirection::Out),
            )
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>();
        for k in overlays {
            self.peerset.connection_overlays.remove(&k).unwrap();
        }
        self.peerset.num_established_connections -= 1;
        match connection.ty {
            ConnectionTy::Connected { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }
}

/// Access to a connection in the [`Peerset`].
pub struct PendingMut<'a, TPeer, TConn, TPending, TSub, TPendingSub> {
    peerset: &'a mut Peerset<TPeer, TConn, TPending, TSub, TPendingSub>,
    id: ConnectionId,
}

impl<'a, TPeer, TConn, TPending, TSub, TPendingSub>
    PendingMut<'a, TPeer, TConn, TPending, TSub, TPendingSub>
{
    /// [`PeerId`] the connection is trying to connect to.
    pub fn peer_id(&self) -> &PeerId {
        let index = self.peerset.connections[self.id.0].peer_index;
        &self.peerset.peers[index].peer_id
    }

    /// Address the connection is trying to reach.
    pub fn address(&self) -> &Multiaddr {
        match &self.peerset.connections[self.id.0].ty {
            ConnectionTy::Pending { target, .. } => target,
            _ => unreachable!(),
        }
    }

    /// Turns this pending connection into an established connection by applying `map` on the
    /// user data.
    pub fn into_established(
        self,
        map: impl FnOnce(TPending) -> TConn,
    ) -> ConnectionMut<'a, TPeer, TConn, TPending, TSub, TPendingSub> {
        let connec = self.peerset.connections.get_mut(self.id.0).unwrap();
        let old_user_data = match mem::replace(&mut connec.ty, ConnectionTy::Poisoned) {
            ConnectionTy::Pending { user_data, .. } => user_data,
            _ => unreachable!(),
        };
        let new_user_data = map(old_user_data);
        connec.ty = ConnectionTy::Connected {
            user_data: new_user_data,
            inbound: false,
        };
        self.peerset.num_established_connections += 1;
        ConnectionMut {
            peerset: self.peerset,
            id: ConnectionId(self.id.0),
        }
    }

    /// Gives access to the user data associated with the connection.
    pub fn user_data_mut(&mut self) -> &mut TPending {
        match &mut self.peerset.connections[self.id.0].ty {
            ConnectionTy::Pending { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }

    /// Gives access to the user data associated with the connection.
    pub fn into_user_data(self) -> &'a mut TPending {
        match &mut self.peerset.connections[self.id.0].ty {
            ConnectionTy::Pending { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }

    /// Removes the pending connection from the data structure.
    pub fn remove(self) -> TPending {
        self.remove_inner(false)
    }

    /// Same as [`PendingMut::remove`], but additionally removes the target address from the list
    /// of known addresses of this node.
    pub fn remove_and_purge_address(self) -> TPending {
        self.remove_inner(true)
    }

    fn remove_inner(self, purge_addr: bool) -> TPending {
        let connection = self.peerset.connections.remove(self.id.0);
        let _was_in = self
            .peerset
            .peer_connections
            .remove(&(connection.peer_index, self.id.0));
        debug_assert!(_was_in);
        let (user_data, address) = match connection.ty {
            ConnectionTy::Pending { user_data, target } => (user_data, target),
            _ => unreachable!(),
        };

        debug_assert_eq!(
            self.peerset
                .connection_overlays
                .range_mut(
                    (self.id.0, 0, SubstreamDirection::In)
                        ..=(self.id.0, usize::max_value(), SubstreamDirection::Out),
                )
                .count(),
            0
        );

        if purge_addr {
            let addrs = &mut self
                .peerset
                .peers
                .get_mut(connection.peer_index)
                .unwrap()
                .addresses;
            let pos = addrs.iter().position(|a| *a == address).unwrap();
            addrs.remove(pos);
            // TODO: remove peer if addrs is empty?
        }

        user_data
    }
}

/// Access to a node in the [`Peerset`].
pub enum NodeMut<'a, TPeer, TConn, TPending, TSub, TPendingSub> {
    /// Node is already known to the data structure.
    Known(NodeMutKnown<'a, TPeer, TConn, TPending, TSub, TPendingSub>),
    /// Node isn't known by the data structure.
    Unknown(NodeMutUnknown<'a, TPeer, TConn, TPending, TSub, TPendingSub>),
}

impl<'a, TPeer, TConn, TPending, TSub, TPendingSub>
    NodeMut<'a, TPeer, TConn, TPending, TSub, TPendingSub>
{
    /// If [`NodeMut::Unknown`], calls the passed closure in order to obtain a user data and
    /// inserts the node in the data structure.
    pub fn or_insert_with(
        self,
        insert: impl FnOnce() -> TPeer,
    ) -> NodeMutKnown<'a, TPeer, TConn, TPending, TSub, TPendingSub> {
        match self {
            NodeMut::Known(k) => k,
            NodeMut::Unknown(k) => k.insert(insert()),
        }
    }

    /// Shortcut for `or_insert_with(Default::default)`.
    pub fn or_default(self) -> NodeMutKnown<'a, TPeer, TConn, TPending, TSub, TPendingSub>
    where
        TPeer: Default,
    {
        self.or_insert_with(Default::default)
    }

    /// Shortcut method. If [`NodeMut::Known`], returns a `Some` containing it.
    pub fn into_known(self) -> Option<NodeMutKnown<'a, TPeer, TConn, TPending, TSub, TPendingSub>> {
        match self {
            NodeMut::Known(k) => Some(k),
            NodeMut::Unknown(_) => None,
        }
    }
}

/// Access to a node is already known to the data structure.
pub struct NodeMutKnown<'a, TPeer, TConn, TPending, TSub, TPendingSub> {
    peerset: &'a mut Peerset<TPeer, TConn, TPending, TSub, TPendingSub>,
    peer_index: usize,
}

impl<'a, TPeer, TConn, TPending, TSub, TPendingSub>
    NodeMutKnown<'a, TPeer, TConn, TPending, TSub, TPendingSub>
{
    /// Returns the network identity of the node.
    pub fn peer_id(&self) -> &PeerId {
        &self.peerset.peers[self.peer_index].peer_id
    }

    /// Adds in the data structure an inbound connection with this node.
    pub fn add_inbound_connection(&mut self, connection: TConn) -> ConnectionId {
        let index = self.peerset.connections.insert(Connection {
            peer_index: self.peer_index,
            ty: ConnectionTy::Connected {
                user_data: connection,
                inbound: true,
            },
        });

        debug_assert_eq!(
            self.peerset
                .connection_overlays
                .range_mut(
                    (index, 0, SubstreamDirection::In)
                        ..=(index, usize::max_value(), SubstreamDirection::Out),
                )
                .count(),
            0
        );

        let _newly_inserted = self
            .peerset
            .peer_connections
            .insert((self.peer_index, index));
        debug_assert!(_newly_inserted);

        ConnectionId(index)
    }

    // TODO: what if Multiaddr isn't in known addresses? do we add it?
    pub fn add_outbound_attempt(
        &mut self,
        target: Multiaddr,
        connection: TPending,
    ) -> ConnectionId {
        let index = self.peerset.connections.insert(Connection {
            peer_index: self.peer_index,
            ty: ConnectionTy::Pending {
                user_data: connection,
                target,
            },
        });

        debug_assert_eq!(
            self.peerset
                .connection_overlays
                .range_mut(
                    (index, 0, SubstreamDirection::In)
                        ..=(index, usize::max_value(), SubstreamDirection::Out),
                )
                .count(),
            0
        );

        let _newly_inserted = self
            .peerset
            .peer_connections
            .insert((self.peer_index, index));
        debug_assert!(_newly_inserted);

        ConnectionId(index)
    }

    /// Returns an iterator to the list of current established connections to that node.
    pub fn connections<'b>(&'b self) -> impl Iterator<Item = ConnectionId> + 'b {
        self.peerset
            .peer_connections
            .range((self.peer_index, 0)..=(self.peer_index, usize::max_value()))
            .map(|(_, i)| *i)
            .filter(move |idx| {
                matches!(
                    self.peerset.connections[*idx].ty,
                    ConnectionTy::Connected { .. }
                )
            })
            .map(ConnectionId)
    }

    /// Returns an iterator to the list of current pending connections to that node.
    pub fn pending_connections<'b>(&'b self) -> impl Iterator<Item = ConnectionId> + 'b {
        self.peerset
            .peer_connections
            .range((self.peer_index, 0)..=(self.peer_index, usize::max_value()))
            .map(|(_, i)| *i)
            .filter(move |idx| {
                matches!(
                    self.peerset.connections[*idx].ty,
                    ConnectionTy::Pending { .. }
                )
            })
            .map(ConnectionId)
    }

    /// Adds an address to the list of addresses the node is reachable through.
    ///
    /// Has no effect if this address is already in the list.
    pub fn add_known_address(&mut self, address: Multiaddr) {
        let list = &mut self.peerset.peers[self.peer_index].addresses;
        if list.iter().any(|a| *a == address) {
            return;
        }

        list.push(address);
    }

    /// Removes an address from the list of known addresses.
    ///
    /// Returns `Ok` if this address was in the list and was removed. Returns `Err` if the address
    /// wasn't in the list.
    // TODO: must not remove if pending connection to this address
    pub fn remove_known_address(&mut self, address: &Multiaddr) -> Result<(), ()> {
        let addresses = &mut self.peerset.peers[self.peer_index].addresses;
        if let Some(pos) = addresses.iter().position(|a| a == address) {
            addresses.remove(pos);
            Ok(())
        } else {
            Err(())
        }
    }

    /// Returns an iterator to the list of addresses known for this peer.
    pub fn known_addresses<'b>(&'b self) -> impl ExactSizeIterator<Item = &'b Multiaddr> + 'b {
        self.peerset.peers[self.peer_index].addresses.iter()
    }

    /// Returns an iterator to the list of addresses known for this peer.
    ///
    /// Filters out addresses to which there is an ongoing connection attempt.
    pub fn known_addresses_no_pending<'b>(&'b self) -> impl Iterator<Item = &'b Multiaddr> + 'b {
        self.known_addresses().filter(move |addr| {
            !self
                .pending_connections()
                .any(move |idx| match &self.peerset.connections[idx.0].ty {
                    ConnectionTy::Pending { target, .. } => target == *addr,
                    _ => false,
                })
        })
    }

    /// Adds the node to an overlay network.
    ///
    /// Has no effect if this node is already in this overlay network.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn add_to_overlay(&mut self, overlay_network_index: usize) {
        assert!(overlay_network_index < self.peerset.num_overlay_networks);
        self.peerset
            .peers_overlays
            .insert((self.peer_index, overlay_network_index));
        self.peerset
            .overlay_peers
            .insert((overlay_network_index, self.peer_index));
    }

    /// Removes the node from an overlay network.
    ///
    /// Returns `true` if the node was indeed part of this overlay network.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn remove_from_overlay(&mut self, overlay_network_index: usize) -> bool {
        assert!(overlay_network_index < self.peerset.num_overlay_networks);
        let was_in1 = self
            .peerset
            .peers_overlays
            .remove(&(self.peer_index, overlay_network_index));
        let was_in2 = self
            .peerset
            .overlay_peers
            .remove(&(overlay_network_index, self.peer_index));
        debug_assert_eq!(was_in1, was_in2);
        was_in1
    }

    /// Gives access to the user data associated with the node.
    pub fn user_data_mut(&mut self) -> &mut TPeer {
        &mut self.peerset.peers[self.peer_index].user_data
    }

    /// Gives access to the user data associated with the node.
    pub fn into_user_data(self) -> &'a mut TPeer {
        &mut self.peerset.peers[self.peer_index].user_data
    }
}

/// Access to a node that isn't known to the data structure.
pub struct NodeMutUnknown<'a, TPeer, TConn, TPending, TSub, TPendingSub> {
    peerset: &'a mut Peerset<TPeer, TConn, TPending, TSub, TPendingSub>,
    peer_id: PeerId,
}

impl<'a, TPeer, TConn, TPending, TSub, TPendingSub>
    NodeMutUnknown<'a, TPeer, TConn, TPending, TSub, TPendingSub>
{
    /// Returns the [`PeerId`] of that node.
    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    /// Inserts the node into the data structure. Returns a [`NodeMutKnown`] for that node.
    pub fn insert(
        self,
        user_data: TPeer,
    ) -> NodeMutKnown<'a, TPeer, TConn, TPending, TSub, TPendingSub> {
        let peer_index = self.peerset.peers.insert(Peer {
            peer_id: self.peer_id.clone(),
            user_data,
            addresses: Vec::new(),
        });

        let _was_in = self.peerset.peer_ids.insert(self.peer_id, peer_index);
        debug_assert!(_was_in.is_none());

        NodeMutKnown {
            peerset: self.peerset,
            peer_index,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn substream_direction_order() {
        // A lot of code above assumes that `In` < `Out`.
        assert!(super::SubstreamDirection::In < super::SubstreamDirection::Out);
    }
}
