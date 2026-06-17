# Security Policy

## Supported versions

Only the latest released version of `drm3-rpc-pool` is supported with security
fixes.

## Reporting a vulnerability

Please report security issues privately. Do **not** open a public GitHub issue
for a vulnerability.

Email **support@drm3.io** with a description of the issue, steps to reproduce,
and any relevant impact assessment. We will acknowledge your report and work
with you on a fix and coordinated disclosure.

## Exposing the proxy

The proxy daemon has **no authentication of its own**. With the default `listen = "127.0.0.1:8545"`, only local processes can reach it, which is safe.

If you bind a public interface (`0.0.0.0`) or otherwise expose the port, you create an **unauthenticated open relay to your configured upstream providers** - anything that can reach the address can spend your keyed/paid RPC quota. Only do this behind a firewall, a private network, or your own auth layer (for example a reverse proxy that enforces authentication). Treat a public `0.0.0.0` bind the same way you would treat a leaked API key.
