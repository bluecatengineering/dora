# dhcp-loadtest

Async DHCP load and integration test client for `dora`, built on `dhcproto`.

## Quick usage

```bash
cargo run -p dhcp-loadtest -- \
  --iface eth2 \
  --clients 1000 \
  --protocol both \
  --concurrency 256 \
  --ramp-per-sec 200 \
  --timeout-ms 1200 \
  --retries 2 \
  --renew \
  --json
```

## VM invocation example

Use explicit server endpoints in integration VMs when available:

```bash
cargo run -p dhcp-loadtest -- \
  --iface eth2 \
  --clients 300 \
  --protocol both \
  --server-v4 192.168.2.1:67 \
  --server-v6 "[2001:db8:2::1]:547" \
  --concurrency 96 \
  --ramp-per-sec 80 \
  --timeout-ms 1500 \
  --retries 3 \
  --renew --release
```

## Dry run

Validate config and deterministic identity generation without sending packets:

```bash
cargo run -p dhcp-loadtest -- \
  --iface eth2 \
  --clients 50 \
  --protocol both \
  --dry-run --json
```
