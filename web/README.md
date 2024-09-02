# Avail Light Client (Web)

## Compile

`wasm-pack build --target web --dev`

## Run

`cp www/index.html pkg/`
`cd pkg`
`python3 -m http.server --directory .`
