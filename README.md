# Coding rules

- No global variables. No thread-local variables. No global logger or anything similar.
- No sleeping a thread. Everything asynchronous.
- Panic as soon as something weird is detected. Don't try to continue running the program if we detect a state inconsistency.
- Code sharing between components kept to a minimum. Code must be isolated as much as possible.
- Things like Prometheus or RPC endpoints must not be rooted deep in the code. It must theoretically be easy to remove support for Prometheus or the RPC server from this library. Prefer pulling information from components from a higher-level rather than passing `Arc` objects around.
- Rustfmt is used.
- Must work in no_std environment.
- Avoid C/C++ code as much as possible.
- TODOs in the code are allowed in modules whose writing is in progress.
