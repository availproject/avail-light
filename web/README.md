# Avail Light Client (Web)

## Compile

`wasm-pack build --target web --release`

NOTE: on MacOS builds, use clang from LLVM installation due the issue with `blst` crate.

## Run page

`cp www/index.html pkg/`
`cp www/avail-light.js pkg/`
`cd pkg`
`python3 -m http.server --directory .`

# Start LC

- Go to http://localhost:8000

## Browser extension

Manifest V3 extension configuration which uses service worker to access latest block confidence is in the `www/extension/`.
To test it, copy folder content to the `pkg/` folder and use it as install folder.
