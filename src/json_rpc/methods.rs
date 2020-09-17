//! List of requests and how to answer them.

use super::parse;

/// Parses a JSON call (usually received from a JSON-RPC server).
///
/// On success, returns a JSON-encoded identifier for that request that must be passed back when
/// emitting the response.
pub fn parse_json_call(message: &str) -> Result<(&str, MethodCall), ParseError> {
    let call_def = parse::parse_call(message).map_err(ParseError::JsonRpcParse)?;

    // No notifications are supported by this server.
    let id = match call_def.id_json {
        Some(id) => id,
        None => return Err(ParseError::UnknownNotification(call_def.method.to_owned())),
    };

    let call = match MethodCall::from_defs(&call_def.method, call_def.params_json) {
        Some(call) => call,
        None => return Err(ParseError::UnknownMethod(call_def.method.to_owned())),
    };

    Ok((id, call))
}

/// Error produced by [`parse_json_call`].
#[derive(Debug, derive_more::Display)]
pub enum ParseError {
    /// Could not parse the body of the message as a valid JSON-RPC message.
    JsonRpcParse(parse::ParseError),
    /// Call concerns a method that isn't recognized.
    UnknownMethod(String),
    /// Call concerns a notification that isn't recognized.
    UnknownNotification(String),
}

/// Could not parse the body of the message as a valid JSON-RPC message.
#[derive(Debug, derive_more::Display)]
pub struct JsonRpcParseError(serde_json::Error);

/// Generates the [`MethodCall`] and [`Response`] enums based on the list of supported requests.
macro_rules! define_methods {
    ($($name:ident($($p_name:ident: $p_ty:ty),*) -> $ret_ty:ty $([$($alias:ident),*])*,)*) => {
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone)]
        pub enum MethodCall {
            $(
                $name {
                    $($p_name: $p_ty),*
                },
            )*
        }

        impl MethodCall {
            /// Returns a list of RPC method names of all the methods in the [`MethodCall`] enum.
            pub fn method_names() -> impl ExactSizeIterator<Item = &'static str> {
                [$(stringify!($name)),*].iter().copied()
            }

            fn from_defs(name: &str, params: &str) -> Option<Self> {
                $(
                    if name == stringify!($name) $($(|| name == stringify!($alias))*)* {
                        #[derive(serde::Deserialize)]
                        struct Params {
                            $(
                                $p_name: $p_ty,
                            )*
                        }

                        if let Ok(params) = serde_json::from_str(params) {
                            let Params { $($p_name),* } = params;
                            return Some(MethodCall::$name {
                                $($p_name,)*
                            })
                        } else {
                            todo!() // TODO: ?
                        }
                    }
                )*

                None
            }
        }

        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone)]
        pub enum Response {
            $(
                $name($ret_ty),
            )*
        }

        impl Response {
            /// Serializes the response into a JSON string.
            ///
            /// `id_json` must be a valid JSON-formatted request identifier, the same the user
            /// passed in the request.
            ///
            /// # Panic
            ///
            /// Panics if `id_json` isn't valid JSON.
            ///
            pub fn to_json_response(&self, id_json: &str) -> String {
                match self {
                    $(
                        Response::$name(out) => {
                            let result_json = serde_json::to_string(&serde_json::Value::from(out.clone())).unwrap();
                            parse::build_success_response(id_json, &result_json)
                        },
                    )*
                }
            }
        }
    };
}

// TODO: some of these methods have aliases
define_methods! {
    account_nextIndex() -> (), // TODO:
    author_hasKey() -> (), // TODO:
    author_hasSessionKeys() -> (), // TODO:
    author_insertKey() -> (), // TODO:
    author_pendingExtrinsics() -> (), // TODO:
    author_removeExtrinsic() -> (), // TODO:
    author_rotateKeys() -> (), // TODO:
    author_submitAndWatchExtrinsic() -> (), // TODO:
    author_submitExtrinsic() -> (), // TODO:
    author_unwatchExtrinsic() -> (), // TODO:
    babe_epochAuthorship() -> (), // TODO:
    chain_getBlock(hash: Option<HashHexString>) -> (), // TODO: bad return type
    chain_getBlockHash(height: u64) -> HashHexString [chain_getHead], // TODO: wrong param
    chain_getFinalizedHead() -> HashHexString [chain_getFinalisedHead],
    chain_getHeader(hash: Option<HashHexString>) -> (), // TODO: bad return type
    chain_subscribeAllHeads() -> String,
    chain_subscribeFinalizedHeads() -> String [chain_subscribeFinalisedHeads],
    chain_subscribeNewHeads() -> String [subscribe_newHead, chain_subscribeNewHead],
    chain_unsubscribeAllHeads(subscription: String) -> bool,
    chain_unsubscribeFinalizedHeads(subscription: String) -> bool [chain_unsubscribeFinalisedHeads],
    chain_unsubscribeNewHeads(subscription: String) -> bool [unsubscribe_newHead, chain_unsubscribeNewHead],
    childstate_getKeys() -> (), // TODO:
    childstate_getStorage() -> (), // TODO:
    childstate_getStorageHash() -> (), // TODO:
    childstate_getStorageSize() -> (), // TODO:
    grandpa_roundState() -> (), // TODO:
    offchain_localStorageGet() -> (), // TODO:
    offchain_localStorageSet() -> (), // TODO:
    payment_queryInfo() -> (), // TODO:
    rpc_methods() -> RpcMethods,
    state_call() -> () [state_callAt],
    state_getKeys() -> (), // TODO:
    state_getKeysPaged() -> () [state_getKeysPagedAt], // TODO:
    state_getMetadata() -> HexString,
    state_getPairs() -> (), // TODO:
    state_getReadProof() -> (), // TODO:
    state_getRuntimeVersion() -> RuntimeVersion [chain_getRuntimeVersion],
    state_getStorage() -> () [state_getStorageAt], // TODO:
    state_getStorageHash() -> () [state_getStorageHashAt], // TODO:
    state_getStorageSize() -> () [state_getStorageSizeAt], // TODO:
    state_queryStorage() -> (), // TODO:
    state_queryStorageAt() -> (), // TODO:
    state_subscribeRuntimeVersion() -> String [chain_subscribeRuntimeVersion],
    state_subscribeStorage() -> String [state_unsubscribeStorage],
    state_unsubscribeRuntimeVersion() -> bool [chain_unsubscribeRuntimeVersion],
    system_accountNextIndex() -> (), // TODO:
    system_addReservedPeer() -> (), // TODO:
    system_chain() -> String,
    system_chainType() -> String,
    system_dryRun() -> () [system_dryRunAt], // TODO:
    system_health() -> SystemHealth,
    system_localListenAddresses() -> Vec<String>,
    system_localPeerId() -> String,
    system_name() -> String,
    system_networkState() -> (), // TODO:
    system_nodeRoles() -> (), // TODO:
    system_peers() -> (), // TODO:
    system_properties() -> serde_json::Map<String, serde_json::Value>,
    system_removeReservedPeer() -> (), // TODO:
    system_version() -> String,
}

#[derive(Debug, Clone)]
pub struct HexString(pub Vec<u8>);

#[derive(Debug, Clone)]
pub struct HashHexString(pub [u8; 32]);

// TODO: not great for type in public API
impl<'a> serde::Deserialize<'a> for HashHexString {
    fn deserialize<D>(deserializer: D) -> Result<HashHexString, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let string = String::deserialize(deserializer)?;

        if !string.starts_with("0x") {
            return Err(serde::de::Error::custom("hash doesn't start with 0x"));
        }

        let bytes = hex::decode(&string[2..]).map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("hash of the wrong length"));
        }

        let mut out = [0; 32];
        out.copy_from_slice(&bytes);
        Ok(HashHexString(out))
    }
}

#[derive(Debug, Clone)]
pub struct RpcMethods {
    pub version: u64,
    pub methods: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeVersion {
    pub spec_name: String,
    pub impl_name: String,
    pub authoring_version: u64,
    pub spec_version: u64,
    pub impl_version: u64,
    pub transaction_version: u64,
    // TODO: apis missing
}

#[derive(Debug, Clone)]
pub struct SystemHealth {
    pub is_syncing: bool,
    pub peers: u64,
    pub should_have_peers: bool,
}

// TODO: implement serde::Serialize instead?
impl From<HashHexString> for serde_json::Value {
    fn from(str: HashHexString) -> serde_json::Value {
        serde_json::Value::String(format!("0x{}", hex::encode(&str.0[..])))
    }
}

// TODO: implement serde::Serialize instead?
impl From<HexString> for serde_json::Value {
    fn from(str: HexString) -> serde_json::Value {
        serde_json::Value::String(format!("0x{}", hex::encode(&str.0[..])))
    }
}

// TODO: implement serde::Serialize instead?
impl From<RpcMethods> for serde_json::Value {
    fn from(methods: RpcMethods) -> serde_json::Value {
        serde_json::Value::Object(
            [
                (
                    "version".to_owned(),
                    serde_json::Value::Number(methods.version.into()),
                ),
                (
                    "methods".to_owned(),
                    serde_json::Value::Array(
                        methods
                            .methods
                            .iter()
                            .map(|s| serde_json::Value::String(s.clone()))
                            .collect(),
                    ),
                ),
            ]
            .iter()
            .cloned() // TODO: that cloned() is crappy; Rust is adding proper support for arrays at some point
            .collect(),
        )
    }
}

// TODO: implement serde::Serialize instead?
impl From<RuntimeVersion> for serde_json::Value {
    fn from(rt: RuntimeVersion) -> serde_json::Value {
        // TODO: not sure about the camelCasing
        #[derive(serde::Serialize)]
        struct SerdeRuntimeVersion {
            spec_name: String,
            impl_name: String,
            authoring_version: u64,
            spec_version: u64,
            impl_version: u64,
            transaction_version: u64,
        }

        serde_json::to_value(SerdeRuntimeVersion {
            spec_name: rt.spec_name,
            impl_name: rt.impl_name,
            authoring_version: rt.authoring_version,
            spec_version: rt.spec_version,
            impl_version: rt.impl_version,
            transaction_version: rt.transaction_version,
        })
        .unwrap()
    }
}

// TODO: implement serde::Serialize instead?
impl From<SystemHealth> for serde_json::Value {
    fn from(health: SystemHealth) -> serde_json::Value {
        serde_json::Value::Object(
            [
                (
                    "isSyncing".to_owned(),
                    serde_json::Value::Bool(health.is_syncing),
                ),
                (
                    "peers".to_owned(),
                    serde_json::Value::Number(From::from(health.peers)),
                ),
                (
                    "shouldHavePeers".to_owned(),
                    serde_json::Value::Bool(health.should_have_peers),
                ),
            ]
            .iter()
            .cloned() // TODO: that cloned() is crappy; Rust is adding proper support for arrays at some point
            .collect(),
        )
    }
}
