#!/bin/sh

echo "Avail-light-client version: $(./avail-light-client --version)"

echo ""

# Run the light-client
echo "## Running Avail-light-client"

./avail-light-client "$@"

