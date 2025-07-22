# Central repository for the Avail Light Clients

[![Project](https://img.shields.io/badge/project-Avail-cyan)](https://www.availproject.org/) [![Build status](https://github.com/availproject/avail-light/actions/workflows/default.yml/badge.svg)](https://github.com/availproject/avail-light/actions/workflows/default.yml) [![Code coverage](https://codecov.io/gh/availproject/avail-light/branch/main/graph/badge.svg?token=7O2EA7QMC2)](https://codecov.io/gh/availproject/avail-light)

This repository is the central place for the development of the [Avail Light Client](https://docs.availproject.org/docs/operate-a-node/node-types).

## Getting started

- **Main documentation** can be found on <https://docs.availproject.org>.
- Want to run a **Light Client node**? Please check this [how-to](https://docs.availproject.org/docs/operate-a-node/run-a-light-client/0010-light-client).
- Detailed light client [readme](client/README.md)
- To report bugs, suggest improvements, or request new features, please open an issue on this repository's GitHub page.

## Repository Structure

The main components of this repository are structured as follows:

- `bootstrap/`: Implementation of protocols that serve as the entry point to the P2P network of Light Clients.
- `client/`: Implements all necessary components for the functioning of the Light Client.
- `core/`: This library forms the core of Avail Light Client, providing essential clients and logic for the system.
- `compatibility-test/`: This folder contains compatibility tests.
- `crawler/` : Diagnostic tool that crawls Light Client's P2P network.
- `fat/` : Pushes grid data to the P2P network to ensure high hit rates.
- `web/`: Web implementation of the light client.
