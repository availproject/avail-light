//! Serde definitions for parsing and emitting JSON-RPC requests.
//!
//! Everything here is an implementation detail of the parent module, and is consequently
//! intentionally marked as `pub(super)`.

use core::fmt;

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub(super) enum SerdeCall {
    MethodCall(SerdeMethodCall),
    Notification(SerdeNotification),
}

#[derive(Debug, PartialEq, Clone, Hash, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
pub(super) enum SerdeId {
    Num(u64),
    Str(String),
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SerdeMethodCall {
    pub(super) jsonrpc: SerdeVersion,
    pub(super) method: String,
    #[serde(default = "default_params")]
    pub(super) params: serde_json::Value,
    pub(super) id: SerdeId,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SerdeNotification {
    pub(super) jsonrpc: SerdeVersion,
    pub(super) method: String,
    #[serde(default = "default_params")]
    pub(super) params: serde_json::Value,
}

fn default_params() -> serde_json::Value {
    serde_json::Value::Null
}

#[derive(Debug, PartialEq, Clone, Copy, Hash, Eq)]
pub(super) enum SerdeVersion {
    V2,
}

impl serde::Serialize for SerdeVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match *self {
            SerdeVersion::V2 => serializer.serialize_str("2.0"),
        }
    }
}

impl<'a> serde::Deserialize<'a> for SerdeVersion {
    fn deserialize<D>(deserializer: D) -> Result<SerdeVersion, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        deserializer.deserialize_identifier(SerdeVersionVisitor)
    }
}

struct SerdeVersionVisitor;

impl<'a> serde::de::Visitor<'a> for SerdeVersionVisitor {
    type Value = SerdeVersion;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        match value {
            "2.0" => Ok(SerdeVersion::V2),
            _ => Err(serde::de::Error::custom("invalid version")),
        }
    }
}

#[derive(Debug, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SerdeSuccess {
    pub(super) jsonrpc: SerdeVersion,
    pub(super) result: serde_json::Value,
    pub(super) id: SerdeId,
}

#[derive(Debug, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SerdeFailure {
    pub(super) version: SerdeVersion,
    pub(super) error: SerdeError,
    pub(super) id: SerdeId,
}

#[derive(Debug, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
pub(super) enum SerdeOutput {
    Success(SerdeSuccess),
    Failure(SerdeFailure),
}

#[derive(Debug, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SerdeError {
    pub(super) code: SerdeErrorCode,
    pub(super) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) data: Option<serde_json::Value>,
}

#[derive(Debug, PartialEq, Clone)]
pub(super) enum SerdeErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    ServerError(i64),
    MethodError(i64),
}

impl<'a> serde::Deserialize<'a> for SerdeErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<SerdeErrorCode, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let code: i64 = serde::Deserialize::deserialize(deserializer)?;

        Ok(match code {
            -32700 => SerdeErrorCode::ParseError,
            -32600 => SerdeErrorCode::InvalidRequest,
            -32601 => SerdeErrorCode::MethodNotFound,
            -32602 => SerdeErrorCode::InvalidParams,
            -32603 => SerdeErrorCode::InternalError,
            -32099..=-32000 => SerdeErrorCode::ServerError(code),
            code => SerdeErrorCode::MethodError(code),
        })
    }
}

impl serde::Serialize for SerdeErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let code = match *self {
            SerdeErrorCode::ParseError => -32700,
            SerdeErrorCode::InvalidRequest => -32600,
            SerdeErrorCode::MethodNotFound => -32601,
            SerdeErrorCode::InvalidParams => -32602,
            SerdeErrorCode::InternalError => -32603,
            SerdeErrorCode::ServerError(code) => code,
            SerdeErrorCode::MethodError(code) => code,
        };

        serializer.serialize_i64(code)
    }
}
