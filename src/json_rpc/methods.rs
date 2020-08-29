//! List of requests and how to answer them.

mod defs;

/// Parses a JSON call (usually received from a JSON-RPC server).
pub fn parse_json_call(message: &str) -> Result<(RequestId, MethodCall), ParseError> {
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

    Ok((method_call_def.id.into(), call))
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

        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone)]
        pub enum Response {
            $(
                $name($ret_ty),
            )*
        }

        impl Response {
            /// Serializes the response into a JSON string.
            pub fn to_json_response(&self, id: RequestId) -> String {
                let def = match self {
                    $(
                        Response::$name(out) => {
                            defs::SerdeOutput::Success(defs::SerdeSuccess {
                                jsonrpc: defs::SerdeVersion::V2,
                                result: From::<$ret_ty>::from(out.clone()),
                                id: id.into(),
                            })
                        },
                    )*
                };

                serde_json::to_string(&def).unwrap()
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
    chain_subscribeAllHeads() -> String,
    chain_subscribeFinalizedHeads() -> String [chain_subscribeFinalisedHeads],
    chain_subscribeNewHeads() -> String [subscribe_newHead, chain_subscribeNewHead],
    chain_unsubscribeAllHeads(subscription: String) -> bool,
    chain_unsubscribeFinalizedHeads(subscription: String) -> bool [chain_unsubscribeFinalisedHeads],
    chain_unsubscribeNewHeads(subscription: String) -> bool [unsubscribe_newHead, chain_unsubscribeNewHead],
    childstate_getKeys() -> (),
    childstate_getStorage() -> (),
    childstate_getStorageHash() -> (),
    childstate_getStorageSize() -> (),
    grandpa_roundState() -> (),
    offchain_localStorageGet() -> (),
    offchain_localStorageSet() -> (),
    payment_queryInfo() -> (),
    rpc_methods() -> RpcMethods,
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
    state_subscribeRuntimeVersion() -> String [chain_subscribeRuntimeVersion],
    state_subscribeStorage() -> String [state_unsubscribeStorage],
    state_unsubscribeRuntimeVersion() -> bool [chain_unsubscribeRuntimeVersion],
    system_accountNextIndex() -> (),
    system_addReservedPeer() -> (),
    system_chain() -> (),
    system_chainType() -> (),
    system_dryRun() -> (),
    system_dryRunAt() -> (),
    system_health() -> SystemHealth,
    system_localListenAddresses() -> (),
    system_localPeerId() -> (),
    system_name() -> String,
    system_networkState() -> (),
    system_nodeRoles() -> (),
    system_peers() -> (),
    system_properties() -> (),
    system_removeReservedPeer() -> (),
    system_version() -> String,
}

#[derive(Debug, PartialEq, Clone, Hash, Eq)]
pub enum RequestId {
    Num(u64),
    Str(String),
}

impl From<defs::SerdeId> for RequestId {
    fn from(id: defs::SerdeId) -> RequestId {
        match id {
            defs::SerdeId::Num(n) => RequestId::Num(n),
            defs::SerdeId::Str(s) => RequestId::Str(s),
        }
    }
}

impl From<RequestId> for defs::SerdeId {
    fn from(id: RequestId) -> defs::SerdeId {
        match id {
            RequestId::Num(n) => defs::SerdeId::Num(n),
            RequestId::Str(s) => defs::SerdeId::Str(s),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HashHexString(pub [u8; 32]);

#[derive(Debug, Clone)]
pub struct RpcMethods {
    pub version: u64,
    pub methods: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SystemHealth {
    pub is_syncing: bool,
    pub peers: u64,
    pub should_have_peers: bool,
}

trait FromSerdeJsonValue {
    fn decode(value: &serde_json::Value) -> Option<Self>
    where
        Self: Sized;
}

impl FromSerdeJsonValue for String {
    fn decode(value: &serde_json::Value) -> Option<Self> {
        Some(value.as_str()?.to_owned())
    }
}

impl FromSerdeJsonValue for u64 {
    fn decode(value: &serde_json::Value) -> Option<Self> {
        value.as_u64()
    }
}

impl FromSerdeJsonValue for HashHexString {
    fn decode(value: &serde_json::Value) -> Option<Self> {
        let value = value.as_str()?;
        if !value.starts_with("0x") {
            return None;
        }

        let bytes = hex::decode(&value[2..]).ok()?;
        if bytes.len() != 32 {
            return None;
        }

        let mut out = [0; 32];
        out.copy_from_slice(&bytes);
        Some(HashHexString(out))
    }
}

impl From<HashHexString> for serde_json::Value {
    fn from(str: HashHexString) -> serde_json::Value {
        serde_json::Value::String(format!("0x{}", hex::encode(&str.0[..])))
    }
}

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
