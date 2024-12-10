# Avail Light Client (Web)

## Compile

`wasm-pack build --target web --dev`

## Run

`cp www/index.html pkg/`
`cp www/avail-light.js pkg/`
`cd pkg`
`python3 -m http.server --directory .`

# Start LC

- Go to http://localhost:8000
