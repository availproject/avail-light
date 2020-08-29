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
    pub(super) params: SerdeParams,
    pub(super) id: SerdeId,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SerdeNotification {
    pub(super) jsonrpc: SerdeVersion,
    pub(super) method: String,
    #[serde(default = "default_params")]
    pub(super) params: SerdeParams,
}

fn default_params() -> SerdeParams {
    SerdeParams::None
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
#[serde(untagged)]
pub(super) enum SerdeParams {
    None,
    Array(Vec<serde_json::Value>),
    Map(serde_json::Map<String, serde_json::Value>),
}
