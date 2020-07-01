//! List of requests and how to answer them.

// TODO: some of these methods have aliases

macro_rules! define_methods {
    ($($name:ident,)*) => {
        #[allow(non_camel_case_types)]
        #[derive(Debug, Copy, Clone)]
        pub enum Method {
            $($name,)*
        }

        impl Method {
            /// Returns the list of supported methods.
            pub fn list() -> impl ExactSizeIterator<Item = Self> {
                [$(Self::$name),*].iter().cloned()
            }

            /// Returns the name of the RPC query.
            pub fn name(&self) -> &'static str {
                match self {
                    $(
                        Self::$name => stringify!($name),
                    )*
                }
            }
        }

    };
}

define_methods! {
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
    childstate_getKeys,
    childstate_getStorage,
    childstate_getStorageHash,
    childstate_getStorageSize,
    grandpa_roundState,
    offchain_localStorageGet,
    offchain_localStorageSet,
    payment_queryInfo,
    rpc_methods,
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
}
