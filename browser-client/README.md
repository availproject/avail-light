# WASM based Avail Light Client

## Build instructions

1. [Install](https://rustwasm.github.io/wasm-pack/) `wasm-pack`

2. Build the `wasm-client` from the clients dir:

```bash
wasm-pack build --target web --out-dir static
```

3. Build and start the client launcher

4. Open the URL printed in the logs
