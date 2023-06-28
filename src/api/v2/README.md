# API Version 2

API version 2 is still under development and under the **api-v2** feature toggle.  
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
