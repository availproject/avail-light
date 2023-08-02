# API Version 2

API version 2 is still under development and under the **api-v2** feature toggle.\
To access new endpoints, light client has to be run with:

```sh
cargo run --release --features api-v2
```

Since entire module is under the feature toggle, tests has to be run with:

```sh
cargo test --features api-v2
```

# API reference

## **GET** `/v2/version`

Gets the version of the light client binary, and the version of the compatible network.

Response:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "version": "{version-string}",
  "network_version": "{version-string}"
}
```

- **version** - the Avail Light Client version
- **network_version** - Avail network version supported by the Avail Light Client

# WebSocket API

The Avail Light Client WebSocket API allows real-time communication between a client and a server over a persistent connection, enabling push notifications as an alternative to polling. Web socket API can be used on its own or in combination with HTTP API to enable different pull/push use cases.

## POST `/v2/subscriptions`

Creates subscriptions for given topics. In case of reconnects, the user needs to subscribe again.

Request:

```yaml
POST /v2/subscriptions HTTP/1.1
Host: {light-client-url}
Content-Type: application/json
Content-Length: {content-length}

{
  "topics": ["header-verified", "confidence-achieved", "data-verified"],
  "data_fields": ["data", "raw"]
}
```

Response:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "subscription_id": "{subscription-id}"
}
```

## Topics

- **header-verified** - header finality is verified and header is available
- **confidence-achieved** - confidence is achieved
- **data-verified** - block data is verified and available

### Data fields

Filters **data-verified** message. Optional parameter used when **raw** transaction data is needed. If omitted, only decoded **data** is present in the message.
