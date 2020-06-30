//! RPC servers.

// TODO: write docs

use crate::service;
use core::fmt;
use std::{io, net::SocketAddr};

/*
list of methods (temporary, for reference)

    account_nextIndex,
    author_hasKey,
    author_hasSessionKeys,
    author_insertKey,
    author_pendingExtrinsics,
    author_removeExtrinsic,
    author_rotateKeys,
    author_submitAndWatchExtrinsic,
    author_submitExtrinsic,
    author_unwatchExtrinsic,
    babe_epochAuthorship,
    chain_getBlock,
    chain_getBlockHash,
    chain_getFinalisedHead,
    chain_getFinalizedHead,
    chain_getHead,
    chain_getHeader,
    chain_getRuntimeVersion,
    chain_subscribeAllHeads,
    chain_subscribeFinalisedHeads,
    chain_subscribeFinalizedHeads,
    chain_subscribeNewHead,
    chain_subscribeNewHeads,
    chain_subscribeRuntimeVersion,
    chain_unsubscribeAllHeads,
    chain_unsubscribeFinalisedHeads,
    chain_unsubscribeFinalizedHeads,
    chain_unsubscribeNewHead,
    chain_unsubscribeNewHeads,
    chain_unsubscribeRuntimeVersion,
    childstate_getKeys,
    childstate_getStorage,
    childstate_getStorageHash,
    childstate_getStorageSize,
    grandpa_roundState,
    offchain_localStorageGet,
    offchain_localStorageSet,
    payment_queryInfo,
    state_call,
    state_callAt,
    state_getKeys,
    state_getKeysPaged,
    state_getKeysPagedAt,
    state_getMetadata,
    state_getPairs,
    state_getReadProof,
    state_getRuntimeVersion,
    state_getStorage,
    state_getStorageAt,
    state_getStorageHash,
    state_getStorageHashAt,
    state_getStorageSize,
    state_getStorageSizeAt,
    state_queryStorage,
    state_queryStorageAt,
    state_subscribeRuntimeVersion,
    state_subscribeStorage,
    state_unsubscribeRuntimeVersion,
    state_unsubscribeStorage,
    subscribe_newHead,
    system_accountNextIndex,
    system_addReservedPeer,
    system_chain,
    system_chainType,
    system_dryRun,
    system_dryRunAt,
    system_health,
    system_localListenAddresses,
    system_localPeerId,
    system_name,
    system_networkState,
    system_nodeRoles,
    system_peers,
    system_properties,
    system_removeReservedPeer,
    system_version,
    unsubscribe_newHead
*/

mod raw;

pub struct RpcServers {
    inner: raw::RpcServers<(), ()>,
}

impl RpcServers {
    /// Creates a new empty collection.
    pub fn new() -> Self {
        let config = raw::Config {
            functions: vec![raw::ConfigFunction {
                name: "chain_getBlockHash".into(),
                id: (),
            }],
            subscriptions: Vec::new(),
        };

        RpcServers {
            inner: raw::RpcServers::new(config),
        }
    }

    /// Spawns a new HTTP JSON-RPC server.
    pub async fn spawn_http(&mut self, addr: SocketAddr) -> Result<(), io::Error> {
        self.inner.spawn_http(addr).await
    }

    /// Spawns a new WebSocket JSON-RPC server.
    pub async fn spawn_ws(&mut self, addr: SocketAddr) -> Result<(), io::Error> {
        self.inner.spawn_ws(addr).await
    }

    /// Returns the next event that happened on one of the servers.
    pub async fn next_event<'a>(&'a mut self) -> Event<'a> {
        match self.inner.next_event().await {
            raw::Event::IncomingRequest { local_id, function_id } => Event::Request(IncomingRequest {
                parent: self,
                inner: local_id,
            }),
            raw::Event::RequestedCancelled(local_id) => todo!(),
            raw::Event::NewSubscription { local_id, subscription_id } => todo!(),
            raw::Event::SubscriptionClosed(locla_id) => todo!(),
        }
    }
}

impl fmt::Debug for RpcServers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

/// Event produced by the [`RpcServers`].
#[derive(Debug)]
pub enum Event<'a> {
    /// A request coming from a connected node.
    Request(IncomingRequest<'a>),
}

/// A request from a connected node.
#[derive(Debug)]
pub struct IncomingRequest<'a> {
    parent: &'a mut RpcServers,
    inner: raw::RequestId,
}

impl<'a> IncomingRequest<'a> {
    /// Answers the request using the given [`service::Service`].
    pub fn answer(self, service: &service::Service) {
        todo!()
    }
}
