use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{fmt::Display, net::SocketAddr, str::FromStr};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum SecretKey {
    Seed { seed: String },
    Key { key: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct RuntimeConfig {
    /// Relay HTTP server host name (default: 127.0.0.1).
    pub http_server_host: String,
    /// Relay HTTP server port (default: 7701).
    pub http_server_port: u16,
    /// Log level. See `<https://docs.rs/log/0.4.17/log/enum.LevelFilter.html>` for possible log level values. (default: `INFO`)
    pub log_level: String,
    /// Set to display structured logs in JSON format. Otherwise, plain text format is used. (default: false)
    pub log_format_json: bool,
    /// Secret key for used to generate keypair. Can be either set to `seed` or to `key`.
    /// If set to seed, keypair will be generated from that seed.
    /// If set to key, a valid ed25519 private key must be provided, else the client will fail
    /// If `secret_key` is not set, random seed will be used.
    pub secret_key: Option<SecretKey>,
    /// Sets the listening P2P network service port. (default: 37000)
    pub p2p_port: u16,
    /// Sets application-specific version of the protocol family used by the peer. (default: "/avail_kad/id/1.0.0")
    pub identify_protocol: String,
    /// Sets agent version that is sent to peers in the network. (default: "avail-light-client/rust-client")
    pub identify_agent: String,
    /// OpenTelemetry Collector endpoint (default: http://otelcollector.avail.tools:4317)
    pub ot_collector_endpoint: String,
    /// Defines a period of time in which periodic metric dump events will be repeated. (default: 15s)
    pub metrics_dump_interval: u64,
    pub origin: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            http_server_host: "127.0.0.1".to_owned(),
            http_server_port: 7701,
            log_level: "INFO".to_string(),
            log_format_json: false,
            secret_key: None,
            p2p_port: 37000,
            identify_protocol: "/avail_kad/id/1.0.0".to_string(),
            identify_agent: "avail-light-client/rust-client".to_string(),
            ot_collector_endpoint: "http://otelcollector.avail.tools:4317".to_string(),
            metrics_dump_interval: 15,
            origin: "external".to_string(),
        }
    }
}

pub struct Addr {
    pub host: String,
    pub port: u16,
}

impl From<&RuntimeConfig> for Addr {
    fn from(value: &RuntimeConfig) -> Self {
        Addr {
            host: value.http_server_host.clone(),
            port: value.http_server_port,
        }
    }
}

impl TryInto<SocketAddr> for Addr {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<SocketAddr, Self::Error> {
        SocketAddr::from_str(&format!("{self}")).context("Unable to parse host and port")
    }
}

impl Display for Addr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}
