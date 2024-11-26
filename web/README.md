# Avail Light Client (Web)

## Compile

`wasm-pack build --target web --dev`

## Run

`cp www/index.html pkg/`
`cd pkg`
`python3 -m http.server --directory .`

# Start LC

- Safari: http://localhost:8000/?network=hex&bootstrap=%2Fip4%2F209.38.38.158%2Fudp%2F39001%2Fwebrtc-direct%2Fcerthash%2FuEiCVz-CTCrMq4I2xwW_WznQPML3dos4GNWiXE_fJjvHiIg
- Firefox: 0.0.0.0:8000/?network=hex&bootstrap=%2Fip4%2F209.38.38.158%2Fudp%2F39001%2Fwebrtc-direct%2Fcerthash%2FuEiCVz-CTCrMq4I2xwW_WznQPML3dos4GNWiXE_fJjvHiIg
