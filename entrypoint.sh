#!/bin/bash

da_bin=/da/bin/avail-light

# Generate configuration.
echo "Generate config from template..."
envsubst < /da/config.yaml.template > /tmp/config.yaml

echo "Generated confing at '/tmp/config.yaml'"
echo "===BEGIN==="
cat /tmp/config.yaml
echo "===END==="

echo "Launching Light Client..."
${da_bin} -c /tmp/config.yaml "$@"
