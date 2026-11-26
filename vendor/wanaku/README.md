# Wanaku forward URL redaction backport

Core's Cargo manifest patches two crates from Wanaku revision
[`1a22a9e692e2f50aed56913000be3def38a7ca2c`](https://github.com/wanaku-ai/wanaku/tree/1a22a9e692e2f50aed56913000be3def38a7ca2c).
The local Wanaku fix is commit `3c1ebc8f9ef1943606939598e6281a3a4b6016cb`.
Cargo uses the original Git pin for the other Wanaku crates.

The backport omits addresses from forward registration, startup, and reconnect
logs. Discovery diagnostics use the normalized origin. Discovery errors omit
transport and server error text that can contain the request URL. The client
uses the original URL for every request.

The local manifests resolve Wanaku dependencies through the same Git pin as
Core. Core builds the server library with the admin UI disabled.

To retire this backport, update all Wanaku pins to a revision that includes
the redaction fix. Remove the Cargo patches and the vendored crates. Remove
the vendor copy from the Containerfile. Run the process regression test:

```bash
cargo test --locked --test runtime_integration forward_discovery_redacts_credential_bearing_urls
```
