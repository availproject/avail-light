//! List of requests and how to answer them.

mod defs;

/// Parses a JSON call (usually received from a JSON-RPC server).
pub fn parse_json_call(message: &str) -> Result<MethodCall, ParseError> {
    let call_def: defs::SerdeCall = serde_json::from_str(message)
        .map_err(JsonRpcParseError)
        .map_err(ParseError::JsonRpcParse)?;

    let method_call_def = match call_def {
        defs::SerdeCall::MethodCall(method_call) => method_call,
        defs::SerdeCall::Notification(notification) => {
            return Err(ParseError::UnknownNotification(notification.method))
        }
    };

    let call = match MethodCall::from_defs(&method_call_def.method, &method_call_def.params) {
        Some(call) => call,
        None => return Err(ParseError::UnknownMethod(method_call_def.method)),
    };

    Ok(call)
}

/// Error produced by [`parse_json_call`].
#[derive(Debug, derive_more::Display)]
pub enum ParseError {
    /// Could not parse the body of the message as a valid JSON-RPC message.
    JsonRpcParse(JsonRpcParseError),
    /// Call concerns a method that isn't recognized.
    UnknownMethod(String),
    /// Call concerns a notification that isn't recognized.
    UnknownNotification(String),
}

/// Could not parse the body of the message as a valid JSON-RPC message.
#[derive(Debug, derive_more::Display)]
pub struct JsonRpcParseError(serde_json::Error);

macro_rules! define_methods {
    ($($name:ident($($p_name:ident: $p_ty:ty),*) -> $ret_ty:ty $([$($alias:ident),*])*,)*) => {
        #[allow(non_camel_case_types)]
        #[derive(Debug, Copy, Clone)]
        pub enum MethodCall {
            $(
                $name {
                    $($p_name: $p_ty),*
                },
            )*
        }

        impl MethodCall {
            fn from_defs(name: &str, params: &defs::SerdeParams) -> Option<Self> {
                $(
                    if name == stringify!($name) $($(|| name == stringify!($alias))*)* {
                        let mut _param_num = 0;
                        $(
                            let $p_name: $p_ty = {
                                let json_value = match params {
                                    defs::SerdeParams::None => return None,
                                    defs::SerdeParams::Array(params) => &params.get(_param_num)?,
                                    defs::SerdeParams::Map(params) => params.get(stringify!($p_name))?,
                                };

                                <$p_ty as FromSerdeJsonValue>::decode(json_value)?
                            };
                            _param_num += 1;
                        )*
                        return Some(MethodCall::$name {
                            $($p_name,)*
                        })
                    }
                )*

                None
            }
        }
    };
}

// TODO: some of these methods have aliases
define_methods! {
    account_nextIndex() -> (),
    author_hasKey() -> (),
    author_hasSessionKeys() -> (),
    author_insertKey() -> (),
    author_pendingExtrinsics() -> (),
    author_removeExtrinsic() -> (),
    author_rotateKeys() -> (),
    author_submitAndWatchExtrinsic() -> (),
    author_submitExtrinsic() -> (),
    author_unwatchExtrinsic() -> (),
    babe_epochAuthorship() -> (),
    chain_getBlock() -> (),
    chain_getBlockHash(height: u64) -> HashHexString,
    chain_getFinalizedHead() -> () [chain_getFinalisedHead],
    chain_getHead() -> (),
    chain_getHeader() -> (),
    chain_getRuntimeVersion() -> (),
    chain_subscribeAllHeads() -> (),
    chain_subscribeFinalizedHeads() -> () [chain_subscribeFinalisedHeads],
    chain_subscribeNewHeads() -> () [subscribe_newHead, chain_subscribeNewHead],
    chain_unsubscribeAllHeads() -> (),
    chain_unsubscribeFinalizedHeads() -> () [chain_unsubscribeFinalisedHeads],
    chain_unsubscribeNewHeads() -> () [unsubscribe_newHead, chain_unsubscribeNewHead],
    childstate_getKeys() -> (),
    childstate_getStorage() -> (),
    childstate_getStorageHash() -> (),
    childstate_getStorageSize() -> (),
    grandpa_roundState() -> (),
    offchain_localStorageGet() -> (),
    offchain_localStorageSet() -> (),
    payment_queryInfo() -> (),
    rpc_methods() -> (),
    state_call() -> (),
    state_callAt() -> (),
    state_getKeys() -> (),
    state_getKeysPaged() -> (),
    state_getKeysPagedAt() -> (),
    state_getMetadata() -> (),
    state_getPairs() -> (),
    state_getReadProof() -> (),
    state_getRuntimeVersion() -> (),
    state_getStorage() -> (),
    state_getStorageAt() -> (),
    state_getStorageHash() -> (),
    state_getStorageHashAt() -> (),
    state_getStorageSize() -> (),
    state_getStorageSizeAt() -> (),
    state_queryStorage() -> (),
    state_queryStorageAt() -> (),
    state_subscribeRuntimeVersion() -> () [chain_subscribeRuntimeVersion],
    state_subscribeStorage() -> () [state_unsubscribeStorage],
    state_unsubscribeRuntimeVersion() -> () [chain_unsubscribeRuntimeVersion],
    system_accountNextIndex() -> (),
    system_addReservedPeer() -> (),
    system_chain() -> (),
    system_chainType() -> (),
    system_dryRun() -> (),
    system_dryRunAt() -> (),
    system_health() -> (),
    system_localListenAddresses() -> (),
    system_localPeerId() -> (),
    system_name() -> (),
    system_networkState() -> (),
    system_nodeRoles() -> (),
    system_peers() -> (),
    system_properties() -> (),
    system_removeReservedPeer() -> (),
    system_version() -> String,
}

trait FromSerdeJsonValue {
    fn decode(value: &serde_json::Value) -> Option<Self>
    where
        Self: Sized;
}

impl FromSerdeJsonValue for u64 {
    fn decode(value: &serde_json::Value) -> Option<Self> {
        value.as_u64()
    }
}
