# API Version 1 reference

In case of error, endpoints will return response with `500 Internal Server Error` status code, and descriptive error message.

## **GET** `/v1/mode`

Retrieves the operating mode of the light client. Light client can operate in two different modes, `LightClient` or `AppClient`, depending on configuration of application ID.

### Responses

If operating mode is `LightClient` response is:

> Status code: `200 OK`

```json
"LightClient"
```

In case of `AppClient` mode, response is:

> Status code: `200 OK`

```json
{"AppClient": {app_id}}
```

## **GET** `/v1/latest_block`

Retrieves the latest block processed by the light client.

### Responses

> Status code: `200 OK`

```json
{"latest_block":{block_number}}
```

## **GET** `/v1/confidence/{block_number}`

Given a block number, it returns the confidence computed by the light client for that specific block.

> Path parameters:

- `block_number` - block number (required)

### Responses

In case when confidence is computed:

> Status code: `200 OK`

```json
{ "block": 1, "confidence": 93.75, "serialised_confidence": "5232467296" }
```

If confidence is not computed, and specified block is before the latest processed block:

> Status code: `400 Bad Request`

```json
"Not synced"
```

If confidence is not computed, and specified block is after the latest processed block:

> Status code: `404 Not Found`

```json
"Not found"
```

## **GET** `/v1/appdata/{block_number}`

Given a block number, it retrieves the hex-encoded extrinsics for the specified block, if available. Alternatively, if specified by a query parameter, the retrieved extrinsic is decoded and returned as a base64-encoded string.

> Path parameters:

- `block_number` - block number (required)

> Query parameters:

- `decode` - `true` if decoded extrinsics are requested (boolean, optional, default is `false`)

### Responses

If application data is available, and decode is `false` or unspecified:

> Status code: `200 OK`

```json
{
	"block": 1,
	"extrinsics": [
		"0xc5018400d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d01308e88ca257b65514b7b44fc1913a6a9af6abc34c3d22761b0e425674d68df7de26be1c8533a7bbd01fdb3a8daa5af77df6d3fb0a67cde8241f461f4fe16f188000000041d011c6578616d706c65"
	]
}
```

If application data is available, and decode is `true`:

> Status code: `200 OK`

```json
{ "block": 1, "extrinsics": ["ZXhhbXBsZQ=="] }
```

If application data is not available, and specified block is the latest block:

> Status code: `401 Unauthorized`

```json
"Processing block"
```

If application data is not available, and specified block is not the latest block:

> Status code: `404 Not Found`

```json
"Not found"
```

## **GET** `/v1/status`

Retrieves the status of the latest block processed by the light client.

> Path parameters:

- `block_number` - block number (required)

### Responses

If latest processed block exists, and `app_id` is configured (otherwise, `app_id` is not set):

> Status code: `200 OK`

```json
{ "block_num": 89, "confidence": 93.75, "app_id": 1 }
```

If there are no processed blocks:

> Status code: `404 Not Found`

```json
"Not found"
```
