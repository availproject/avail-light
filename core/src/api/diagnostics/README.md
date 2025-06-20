# P2P Diagnostics API

This API is intended to be used for P2P network observability and diagnostics.

## **GET** `/v1/p2p/local/info`

Returns:

- `peer_id`
- kademlia operation mode for `peer_id`
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

## **POST** `/v1/p2p/peers/dial`

Dials a peer on the light client P2P network and waits for it's response.
If the dial goes through, a 200 OK response, the example JSON is stated below.

If an error occurs on the dial 3 types of responses can be returned:

1. 200 OK with the `dial_error` field set, which contains an error type and description.
2. 400 Bad Request if some of the user inputs are not valid
3. 500 Internal Server Error for other errors

Request:

```yaml
POST /v1/p2p/peers/dial HTTP/1.1
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

## **GET** `/v1/p2p/peers/multiaddress/{peer_id}`

Returns a reachable multiaddress for a peer on the light client P2P network.
If the request goes through, the endpoint sends a 200 OK response, the example JSON is stated below.

In case of an error the following response is received:

1. 400 Bad Request with a message `Peer not found in the routing table or its IP is not public.`

Request:

```yaml
GET /v1/p2p/peers/multiaddress/{target-peers-peer-id} HTTP/1.1
```

Response:

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
  "peer_id": {target-peers-peer-id},
  "multiaddresses": "{target-peers-multi-address}",
  
}
```

## GET `/v1/p2p/dht/{metric}`

Returns a specific DHT performance metric.

### Path Parameters

- `{metric}` (required): The name of the metric to fetch.

  Valid values:
  - `fetch` → Returns the DHT fetched percentage.

### Example: `fetch`

**Request**

```yaml
GET /v1/p2p/dht/fetch
```

**Response**

```yaml
HTTP/1.1 200 OK
Content-Type: application/json

{
	"dht_fetched_percentage": 84.5
}
```

In case of an error (e.g. no data available in the DHT), the following response is returned:

```yaml
HTTP/1.1 404 Not Found
Content-Type: text/plain

Not Found
```

If the requested peer or metric does not exist, or the value cannot be parsed, a 404 will be returned.

## Errors

In case of an error, endpoints will return a response with `500 Internal Server Error` status code, and a descriptive error message:

```yaml
HTTP/1.1 500 Internal Server Error
Content-Type: text/plain

Internal Server Error
```
