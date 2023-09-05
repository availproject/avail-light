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

## **GET** `/v2/status`

Gets current status and active modes of the light client.

Response:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "modes": [
    "light",
    "app",
    "partition"
  ],
  "app_id": {app-id}, // Optional
  "genesis_hash": "{genesis-hash}",
  "network": "{network}",
  "blocks": {
    "latest": {latest},
    "available": { // Optional
      "first": {first},
      "last": {last}
    },
    "app_data": { // Optional
      "first": {first},
      "last": {last}
    },
    "historical_sync": { // Optional
      "synced": false,
      "available": { // Optional
        "first": {first},
        "last": {last}
      },
      "app_data": { // Optional
        "first": {first},
        "last": {last}
      }
    }
  },
  "partition": "{partition}" // Optional
}
```

- **modes** - active modes
- **app_id** - if **app** mode is active, this field contains configured application ID
- **genesis_hash** - genesis hash of the network to which the light client is connected
- **network** - network host, version and spec version light client is currently con
- **blocks** - state of processed blocks
- **partition** - if configured, displays partition which light client distributes to the peer to peer network

### Modes

- **light** - data availability sampling mode, the light client performs random sampling and calculates confidence
- **app** - light client fetches, verifies, and stores application-related data
- **partition** - light client fetches configured block partition and publishes it to the DHT

### Blocks

- **latest** - block number of the latest [finalized](https://docs.substrate.io/learn/consensus/) block received from the node
- **available** - range of blocks with verified data availability (configured confidence has been achieved)
- **app_data** - range of blocks with app data retrieved and verified
- **historical_sync** - state for historical blocks syncing up to configured block (ommited if historical sync is not configured)

### Historical sync

- **synced** - `true` if there are no historical blocks left to sync
- **available** - range of historical blocks with verified data availability (configured confidence has been achieved)
- **app_data** - range of historical blocks with app data retrieved and verified

## POST `/v2/submit`

Submits application data to the avail network.\
In case of `data` transaction, data transaction is created, signed and submitted.\
In case of `extrinsic`, externaly created and signed transaction is submitted. Only one field is allowed per request.\
Both `data` and `extrinsic` has to be encoded using base64 encoding.

Request:

```yaml
POST /v2/submit HTTP/1.1
Host: {light-client-url}
Content-Type: application/json
Content-Length: {content-length}

{
  "data": "{base-64-encoded-data}" // Optional
  "extrinsic": "{base-64-encoded-data}" // Optional
}
```

Response:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "block_hash": "{block-hash}",
  "hash": "{transaction-hash}",
  "index": {transaction-index}
}
```

If **app** mode is not active (or signing key is not configured and `data` is submitted) response is:

```yaml
HTTP/1.1 404 Not found
```

## Errors

In case of an error, endpoints will return a response with `500 Internal Server Error` status code, and descriptive error message:

```yaml
HTTP/1.1 500 Internal Server Error
Content-Type: text/plain

Internal Server Error
```

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
  "data_fields": ["data", "extrinsic"]
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

### Topics

- **header-verified** - header finality is verified and header is available
- **confidence-achieved** - confidence is achieved
- **data-verified** - block data is verified and available

### Data fields

Filters **data-verified** message. Optional parameter used when encoded **extrinsic** is needed. If omitted, only decoded **data** is present in the message.

## GET `/v2/ws/{subscription-id}`

Connects to Avail Light Client web socket. Multiple connections are currently allowed.

## Client-to-server messages

Every request should contain unique **request_id** field, used to correlate request with response.

### Request version

Request Avail Light Client version data.

```json
{
	"type": "version",
	"request_id": "{uuid}"
}
```

### Request status

Request current Avail Light Client status data.

```json
{
	"type": "status",
	"request_id": "{uuid}"
}
```

### Submit data transaction

Submits data transaction to the Avail.

```json
{
	"type": "submit",
	"request_id": "{uuid}",
	"message": {
		"data": "{base-64-encoded-data}", // Optional
		"extrinsic": "{base-64-encoded-data}" // Optional
	}
}
```

## Server-to-client messages

If response contains ******request_id****** field, it will be pushed to the client which initiated request. Those messages are not subject to a topic filtering at the moment.

### Version

Version response.

```json
{
	"topic": "version",
	"request_id": "{uuid}",
	"message": {
		"version": "{version-string}",
		"network_version": "{version-string}"
	}
}
```

### Status

Status response.

```json
{
  "topic": "status",
  "request_id": "{uuid}",
  "message": {
    "modes": [
      "light",
      "app",
      "partition"
    ],
    "app_id": {app-id}, // Optional
    "genesis_hash": "{genesis-hash}",
    "network": "{network}",
    "blocks": {
      "latest": {latest},
      "available": {  // Optional
        "first": {first},
        "last": {last}
      },
      "app_data": {  // Optional
        "first": {first},
        "last": {last}
      },
      "historical_sync": {  // Optional
        "synced": false,
        "available": {  // Optional
          "first": {first},
          "last": {last}
        },
        "app_data": {  // Optional
          "first": {first},
          "last": {last}
        }
      }
    },
    "partition": "{partition}"
  }
}
```

### Data transaction submitted

Data transaction submitted response. It contains transaction **hash** used to correlate transaction with verified data once transaction is included in the block and verified by the light client.

```json
{
  "topic": "data-transaction-submitted",
  "request_id": "{uuid}",
  "message": {
    "block_hash": "{block-hash}",
    "hash": "{transaction-hash}",
    "index": {transaction-index}
  }
}
```

If **app** mode is not active or signing key is not configured error response is sent with descriptive error message.

### Errors

In case of errors, descriptive error message is sent:

```json
{
	"topic": "error",
	"request_id": "{uuid}", // Optional
	"code": "{error-code}",
	"message": "{descriptive-error-message}"
}
```

Error codes:

- **bad-request** - request sent via web socket message is not valid
