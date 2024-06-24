# API Version 2 reference

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
- **historical_sync** - state for historical blocks syncing up to configured block (omitted if historical sync is not configured)

Blocks without data transactions, or blocks that fails to commit to the data, are considered incomplete and commitments will be empty.
Ranges of **available** and **app_data** blocks can contain **incomplete** blocks.

### Historical sync

- **synced** - `true` if there are no historical blocks left to sync
- **available** - range of historical blocks with verified data availability (configured confidence has been achieved)
- **app_data** - range of historical blocks with app data retrieved and verified

## **GET** `/v2/blocks/{block_number}`

Gets specified block status and confidence if applicable.

If **block_number <= latest_block,** then the block is either processed or skipped, and possible statuses are:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "status": "unavailable|pending|verifying-header|verifying-confidence|verifying-data|incomplete|finished",
  "confidence": {confidence} // Optional
}
```

- **status** - block status
- **confidence** - data availability confidence, available if block processing is finished

### Status

- **unavailable** - block will not be processed if
  \
  **latest_block - sync_depth > block_number**
- **pending** - block will be processed at some point in the future if
  \
  **latest_block - sync_depth ≤ block_number ≤ latest_block**
- **verifying-header** - block processing is started, and the header finality is being checked
- **verifying-confidence** - block header is verified and available, confidence is being checked
- **verifying-data** - confidence is achieved, and data is being fetched and verified (if configured)
- **incomplete** - block header is available, but data availability cannot be verified
- **finished** - block header is available, confidence is achieved, and data is available (if configured)

This status does not give information on what is available. In the case of web sockets messages are already pushed, similar to case of the frequent polling, so header and confidence will be available if **verifying-header** and **verifying-confidence** has been successful.

If **block_number > latest_block,** block status cannot yet be derived and the response on this and other endpoints with `/v2/blocks/{block_number}` prefix is:

```yaml
HTTP/1.1 404 Not Found
```

## **GET** `/v2/blocks/{block_number}/header`

Gets the block header if it is available.

If **block_status = "verifying-confidence|verifying-data|incomplete|finished"**, the header is available, and the response is:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "hash": "{hash}",
  "parent_hash": "{parent-hash}",
  "number": {number},
  "state_root": "{state-root}",
  "extrinsics_root": "{extrinsics-root}",
  "extension": {
    "rows": {rows},
    "cols": {cols},
    "data_root": "{data-root}", // Optional
    "commitments": [
      "{commitment}", ...
    ],
    "app_lookup": {
      "size": {size},
      "index": [
        {
          "app_id": {app-id},
          "start": {start}
        }
      ]
    }
  }
}
```

If **block_status = "unavailable|pending|verifying-header"**, header is not available and response is:

```yaml
HTTP/1.1 400 Bad Request
```

## **GET** `/v2/blocks/{block_number}/data?fields=data,extrinsic`

Gets the block data if available. Query parameter `fields` specifies whether to return decoded data and encoded extrinsic (with signature). If `fields` parameter is omitted, response contains **hash** and **data**, while **extrinsic** is omitted.

If **block_status = "finished"**, data is available and the response is:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "data_transactions": [
    {
      "data": "{base-64-encoded-data}" // Optional
      "extrinsic": "{base-64-encoded-extrinsic}", // Optional
    }
  ]
}
```

If **block_status** is not **“finished”**, or **app** mode is not enabled, data is not available and the response is:

```yaml
HTTP/1.1 400 Bad Request
```

## POST `/v2/submit`

Submits application data to the avail network.\
In case of `data` transaction, data transaction is created, signed and submitted.\
In case of `extrinsic`, externally created and signed transaction is submitted. Only one field is allowed per request.\
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
  "block_number": {block-number},
  "block_hash": "{block-hash}",
  "hash": "{transaction-hash}",
  "index": {transaction-index}
}
```

If **app** mode is not active (or signing key is not configured and `data` is submitted) response is:

```yaml
HTTP/1.1 404 Not found
```

# P2P Diagnostics API

This API is intended to be used for P2P network observability and diagnostics.

## **GET** `/v2/p2p/local/info`

Returns:

- `peer_id`
- list of listeners with local addresses
- list of listeners with external addresses
- number of clients found in peers routing table
- number of clients with non-private addresses found in the routing table

External addresses are only populated once confirmed externally by the bootstrap.

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "peer_id": "{local-peer-id}",
  "listeners": {
    "local": [
      "{multi-address}",
      "{multi-address}",
      "{multi-address}"
    ],
    "external": [
      "{multi-address}"
    ],
  },
  "routing_table_peers_count": "{num}",
  "routing_table_external_peers_count": "{num}",
}
```

## **POST** `/v2/p2p/peers/dial`

Dials a peer on the light client P2P network and waits for it's response.
If the dial goes through, a 200 OK response, the example JSON is stated bellow.

If an error occurs on the dial 3 types of responses can be returned:

1. 200 OK with the `dial_error` field set, which contains a error type and description.
2. 400 Bad Request if some of the user inputs are not valid
3. 500 Internal Server Error for other errors

Request:

```yaml
POST /v2/p2p/peers/dial HTTP/1.1
{
  "multiaddress": "{target-peers-multi-address}"
  "peer_id": "{target-peers-peer-id}"
}
```

Response:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "dial_success": {
    "peer_id": {peer_id},
    "multiaddress": "{multi-address}",
    "established_in": "{established_in}", # Time needed for connection establish (in seconds)
    "num_established": {num_established} # Number of concurrent connections established with the peer
  },
  "dial_error": null
}
```

Response containing an error:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "dial_success": null,
  "dial_error": {
    "error": "wrong-peer-id",
    "description": "The peerID obtained on the connection is not matching the one provided. User provided peerID: 12D3KooWBkLsNGaD3SpMaRWtAmWVuiZg1afdNSPbtJ8M8r9ArGRA. Observed peerID: 12D3KooWBkLsNGaD3SpMaRWtAmWVuiZg1afdNSPbtJ8M8r9ArGRT."
  }
}
```

## **POST** `/v2/p2p/peers/get-multiaddress`

Returns a multi address for a peer on the light client P2P network.
If the request goes through, the endpoint sends a 200 OK response, the example JSON is stated bellow.

Request:

```yaml
POST /v2/p2p/peers/get-multiaddress HTTP/1.1
{
  "peer_id": "{target-peers-peer-id}"
}
```

Response:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "peer_id": {peer_id},
  "multiaddress": "{multi-address}",
  
}
```

## Errors

In case of an error, endpoints will return a response with `500 Internal Server Error` status code, and a descriptive error message:

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
    "block_number": {block-number},
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

### Header verified

When header verification is finished, the message is pushed to the light client on a **header-verified** topic:

```json
{
  "topic": "header-verified",
  "message": {
    "block_number": {block-number},
    "header": {
      "hash": "{hash}",
      "parent_hash": "{parent-hash}",
      "number": {number},
      "state_root": "{state-root}",
      "extrinsics_root": "{extrinsics-root}",
      "extension": {
        "rows": {rows},
        "cols": {cols},
        "data_root": "{data-root}", // Optional
        "commitments": [
          "{commitment}", ...
        ],
        "app_lookup": {
          "size": {size},
          "index": [
            {
              "app_id": {app-id},
              "start": {start}
            }
          ]
        }
      }
    }
  }
}
```

### Confidence achieved

When high confidence in data availability is achieved, the message is pushed to the light client on the **confidence-achieved** topic:

```json
{
  "topic": "confidence-achieved",
  "message": {
    "block_number": {block-number},
    "confidence": {confidence} // Optional
  }
}
```

### Data verified

When high confidence in data availability is achieved, the message is pushed to the light client on the **data-verified** topic:

```json
{
 "topic": "data-verified",
 "message": {
  "block_number": {block-number},
  "data_transactions": [{
   "data": "{base-64-encoded-data}", // Optional
   "extrinsic": "{base-64-encoded-extrinsic}" // Optional
  }]
 }
}
```
