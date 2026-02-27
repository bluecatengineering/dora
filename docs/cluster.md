# Dora DHCP -- NATS Clustered Mode

Dora supports running multiple DHCP server instances as an active-active cluster,
coordinated through [NATS JetStream](https://nats.io/) key-value stores. Every
node can serve both DHCPv4 and DHCPv6 traffic independently while sharing lease
state through NATS so that IP assignments are consistent across the cluster.

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Component Map](#component-map)
3. [Configuration](#configuration)
4. [Installation](#installation)
5. [Message Flow: DHCPv4](#message-flow-dhcpv4)
6. [Message Flow: DHCPv6](#message-flow-dhcpv6)
7. [Conflict Resolution](#conflict-resolution)
8. [Degraded Mode](#degraded-mode)
9. [NATS JetStream Storage Layout](#nats-jetstream-storage-layout)
10. [Host Option Overrides](#host-option-overrides)
11. [Prometheus Metrics](#prometheus-metrics)
12. [NixOS Deployment](#nixos-deployment)

---

## Architecture Overview

### Cluster Topology

```
  +------------------------------------------------------------------------+
  |                     NATS JetStream Cluster                             |
  |                                                                        |
  |   +-------------------------+      +-------------------------------+   |
  |   | dora_leases KV          |      | dora_host_options KV         |    |
  |   | (lease records + index) |      | (host option overrides)      |    |
  |   +------------+------------+      +---------------+---------------+   |
  +----------------|------------------------------------|------------------+
                   |                                    |
  +----------------v------------------+   +-------------v------------------+
  | Dora Node A                        |   | Dora Node B                   |
  |                                    |   |                               |
  | MsgType -> StaticAddr ->           |   | MsgType -> StaticAddr ->      |
  | NatsLeases -> HostOptionSync       |   | NatsLeases -> HostOptionSync  |
  |                                    |   |                               |
  | NatsLeases internals:              |   | NatsLeases internals:         |
  | IpManager -> ClusteredBackend ->   |   | IpManager -> ClusteredBackend |
  | LeaseCoordinator (r/w leases KV)   |   | -> LeaseCoordinator (r/w KV)  |
  | HostOptionSync (read options KV)   |   | HostOptionSync (read options) |
  +------------------+-----------------+   +----------------+--------------+
                     |                                      |
                     +--------------- DHCP Clients ---------+
```

### Plugin Pipeline (per request)

Each DHCP request flows through the plugin pipeline sequentially.
The lease and host-option steps talk to independent NATS KV stores:

```
  DHCP Request
      |
      v
  +---------------------------+
  | 1. MsgType                |
  |    classify packet        |
  +-------------+-------------+
                |
                v
  +---------------------------+
  | 2. StaticAddr             |
  |    apply fixed leases     |
  +-------------+-------------+
                |
                v
  +-------------------------------------------------------+
  | 3. NatsLeases                                         |
  |                                                       |
  | local path:   IpManager -> ClusteredBackend           |
  | cluster path: LeaseCoordinator <-> dora_leases KV     |
  +---------------------------+---------------------------+
                              |
                              v
  +-------------------------------------------------------+
  | 4. HostOptionSync                                      |
  | read path: HostOptionSync -> dora_host_options KV      |
  | action: enrich response options                        |
  +---------------------------+---------------------------+
                              |
                              v
                       DHCP Response
```

**Key design principles:**

- **Local-first allocation, cluster-second coordination.** Each node picks an
  IP locally via its own `IpManager`, then validates the choice through NATS.
  If another node already claimed that IP, the local reservation is rolled back
  and a different address is tried.

- **New allocations require NATS; renewals do not.** When NATS is unavailable
  (degraded mode), new clients are blocked but existing clients can still renew
  their leases from the local known-lease cache.

- **Best-effort cleanup.** Release and decline operations always succeed locally.
  NATS coordination for these is fire-and-forget -- failures are logged but never
  block the DHCP response.

- **Host option overrides via KV.** Per-host DHCP options (boot file, next-server,
  etc.) can be injected at runtime by writing entries into the `dora_host_options`
  NATS KV bucket. No server restart required.

---

## Component Map

| Crate / Module | Role |
|---|---|
| `libs/config` | Configuration parsing: `BackendMode`, `NatsConfig`, validation, defaults |
| `libs/nats-coordination` | NATS client, JetStream KV operations, lease & host-option coordination |
| `libs/ip-manager` | Local IP address selection, ping checks, SQLite-backed lease cache |
| `plugins/leases` | Shared DHCPv4 lease plugin: `Leases<B: LeaseStore>` |
| `plugins/nats-leases` | NATS adapter/backend + DHCPv6 clustered plugin (`NatsBackend`, `NatsV6Leases`) |
| `plugins/nats-leases/v6` | DHCPv6 message handler (SOLICIT, REQUEST, RENEW, RELEASE, DECLINE) |
| `plugins/nats-host-options` | Host option enrichment via NATS KV lookup |
| `bin/src/main.rs` | Startup: mode selection, NATS connection, plugin wiring |
| `tools/dhcp-loadtest` | Load testing tool for NATS-mode validation |

---

## Configuration

### Minimal Cluster Config

```yaml
backend_mode: nats

nats:
  servers:
    - "nats://127.0.0.1:4222"

networks:
  192.168.1.0/24:
    ranges:
      192.168.1.100-192.168.1.200:
        lease_time:
          default: 3600
```

Setting `backend_mode: nats` activates the clustered path. Everything else in
the `nats:` block has sensible defaults.

### Full Config Reference

```yaml
backend_mode: nats          # "standalone" (default) or "nats"

nats:
  # Required -- at least one NATS server URL.
  # Comma-separated lists within a single string are also accepted.
  # Schemes: nats://, tls://, ws://, wss://  (bare host:port also works)
  servers:
    - "nats://nats1.example.com:4222"
    - "nats://nats2.example.com:4222"

  # Prefix for all NATS subjects. Changing this automatically derives
  # all subject names below unless they are explicitly overridden.
  subject_prefix: "dora.cluster"          # default

  # Individual subject overrides (rarely needed)
  subjects:
    lease_upsert:           "dora.cluster.lease.upsert"
    lease_release:          "dora.cluster.lease.release"
    lease_snapshot_request:  "dora.cluster.lease.snapshot.request"
    lease_snapshot_response: "dora.cluster.lease.snapshot.response"

  # JetStream KV bucket names
  leases_bucket:        "dora_leases"         # default
  host_options_bucket:  "dora_host_options"    # default

  # Timers
  lease_gc_interval_ms: 60000                 # GC sweep interval (default 60s)
  coordination_state_poll_interval_ms: 1000   # health poll (default 1s)
  connect_timeout_ms:   5000                  # per-attempt timeout (optional)
  connect_retry_max:    10                    # max connection attempts (default)
  request_timeout_ms:   2000                  # per-request timeout (optional)

  # AsyncAPI contract version (for forward compatibility)
  contract_version: "1.0.0"                   # default

  # Authentication (pick one mode)
  security_mode: none                         # default
  # security_mode: user_password
  # username: "dora"
  # password: "secret"
  # security_mode: token
  # token: "s3cret"
  # security_mode: nkey
  # nkey_seed_path: "/etc/dora/nkey.seed"
  # security_mode: tls
  # tls_cert_path: "/etc/dora/cert.pem"
  # tls_key_path:  "/etc/dora/key.pem"
  # tls_ca_path:   "/etc/dora/ca.pem"
  # security_mode: creds_file
  # creds_file_path: "/etc/dora/dora.creds"
```

### CLI / Environment Overrides

| Flag | Env Var | Description |
|---|---|---|
| `--backend-mode` | `DORA_BACKEND_MODE` | Override backend mode (`standalone` or `nats`) |
| `--instance-id` | `DORA_INSTANCE_ID` | Server identity stamped on every lease record |
| `--nats-servers` | `DORA_NATS_SERVERS` | Single NATS server URL override (for multiple servers, use the `nats.servers` list in config) |

`--instance-id` defaults to the value of `--dora-id` if not set.

---

## Installation

### Prerequisites

- A running **NATS server with JetStream enabled** (`nats-server -js`).
  For production, run a 3-node NATS cluster for fault tolerance.
- Two or more Dora instances, all pointing at the same NATS cluster and
  sharing the same network/range configuration.

### Manual Setup

1. Start a NATS server with JetStream:

   ```bash
   nats-server -js -p 4222
   ```

2. Write your Dora config with `backend_mode: nats` (see above).

3. Start each Dora instance with a unique identity:

   ```bash
   # Node A
   dora -c /etc/dora/config.yaml --instance-id node-a

   # Node B
   dora -c /etc/dora/config.yaml --instance-id node-b
   ```

4. On startup each node will:
   - Connect to NATS (with retry and exponential backoff)
   - Create JetStream KV buckets (`dora_leases`, `dora_host_options`) if absent
   - Run a write self-test (write/read/delete a probe key)
   - Begin serving DHCP traffic

### Startup Self-Test

Before accepting DHCP traffic, each node writes a probe key into the
`dora_leases` bucket, reads it back, verifies byte equality, and deletes it.
This validates the full JetStream KV write path. If the self-test fails,
startup is aborted.

---

## Message Flow: DHCPv4

### DISCOVER / OFFER (New Allocation)

```
  Client                  Dora Node               NATS KV (dora_leases)
    │                         │                            │
    │──── DHCPDISCOVER ──────►│                            │
    │                         │                            │
    │                  1. Pick IP locally                   │
    │                  (IpManager.reserve_first)            │
    │                         │                            │
    │                  2. Build LeaseRecord                 │
    │                  (state=Reserved, rev=0)              │
    │                         │                            │
    │                         │──── KV GET ip index ──────►│
    │                         │◄─── (check IP conflict) ───│
    │                         │                            │
    │                         │──── KV PUT lease record ──►│
    │                         │──── KV PUT ip index ──────►│
    │                         │◄─── OK (rev=1) ────────────│
    │                         │                            │
    │                  3. Cache in known_leases             │
    │                         │                            │
    │◄──── DHCPOFFER ────────│                            │
    │      (yiaddr=IP)        │                            │
```

If the client included a requested address (option 50), step 1 uses
`IpManager.try_ip` for that specific IP instead of `reserve_first`.

### REQUEST / ACK (Lease Confirmation)

```
  Client                  Dora Node               NATS KV (dora_leases)
    │                         │                            │
    │──── DHCPREQUEST ───────►│                            │
    │                         │                            │
    │                  1. Renew-cache check                 │
    │                  (skip NATS if recently renewed)      │
    │                         │                            │
    │                  2. Local lease transition             │
    │                  (IpManager.try_lease)                │
    │                         │                            │
    │                         │──── KV PUT lease record ──►│
    │                         │     (state=Leased)          │
    │                         │◄─── OK ────────────────────│
    │                         │                            │
    │                  3. Update known_leases               │
    │                  4. Trigger DDNS update               │
    │                         │                            │
    │◄──── DHCPACK ──────────│                            │
```

### RELEASE / DECLINE

```
  Client                  Dora Node               NATS KV
    │                         │                       │
    │──── DHCPRELEASE ───────►│                       │
    │                         │                       │
    │                  1. Local release (always)       │
    │                  (IpManager.release_ip)          │
    │                         │                       │
    │                  2. Best-effort NATS             │
    │                         │── KV PUT (Released) ─►│
    │                         │   (errors logged      │
    │                         │    but not fatal)      │
```

Decline is similar but sets `state=Probated` and quarantines the IP for
the network's probation period.

---

## Message Flow: DHCPv6

DHCPv6 uses DUID + IAID as the client key (one client can hold multiple
leases with different IAIDs). Address selection uses deterministic SipHash
when the client does not request a specific address.

### SOLICIT / ADVERTISE

```
  Client                  Dora Node               NATS KV (dora_leases)
    │                         │                            │
    │──── DHCPv6 SOLICIT ────►│                            │
    │                         │                            │
    │                  1. Extract DUID + IAID               │
    │                                                      │
    │                  2. Check known_leases cache          │
    │                     (reuse existing if found)         │
    │                                                      │
    │                  3. Pick address:                     │
    │                     - Client hint (IA_ADDR) or       │
    │                     - SipHash(subnet:duid:iaid)      │
    │                         │                            │
    │                         │── KV PUT (Reserved) ──────►│
    │                         │◄── OK ─────────────────────│
    │                         │                            │
    │◄── DHCPv6 ADVERTISE ───│                            │
    │    (IA_NA with address)  │                            │
```

### REQUEST / RENEW / REPLY

```
  Client                  Dora Node               NATS KV
    │                         │                       │
    │── DHCPv6 REQUEST ──────►│                       │
    │                         │                       │
    │                  1. Extract requested addr       │
    │                     from IA_ADDR option          │
    │                         │                       │
    │                         │── KV PUT (Leased) ───►│
    │                         │◄── OK ────────────────│
    │                         │                       │
    │◄── DHCPv6 REPLY ──────│                       │
    │    (IA_NA confirmed)    │                       │
```

Renew follows the same path. In degraded mode, renewals of known leases
are served locally without NATS (see [Degraded Mode](#degraded-mode)).

---

## Conflict Resolution

When two nodes simultaneously try to assign the same IP to different clients,
the NATS KV IP-index check detects the conflict.

### Conflict Flow (reserve_first)

```
  Dora Node A                    NATS KV                    Dora Node B
       │                            │                            │
  1. reserve IP .50 locally         │       1. reserve IP .50 locally
       │                            │                            │
       │── KV PUT ip/.50 ──────────►│◄───────── KV PUT ip/.50 ──│
       │   (lease_key=clientA)       │    (lease_key=clientB)     │
       │                            │                            │
       │◄─ OK (wins write race) ────│──── Conflict (.50 owned ──►│
       │                            │     by clientA)             │
       │                            │                            │
       │                            │    2. Quarantine .50        │
       │                            │       (probation period)    │
       │                            │                            │
       │                            │    3. Pick new IP .51       │
       │                            │                            │
       │                            │◄── KV PUT ip/.51 ──────────│
       │                            │──── OK ───────────────────►│
```

The retry budget is **8 attempts** (`MAX_CONFLICT_RETRIES`). On each conflict:

1. The conflicted IP is placed in **probation** locally via `IpManager.probate_ip`
   so it will not be selected again during the probation period.
2. A fresh IP is allocated from the range.
3. Coordination is retried with the new IP.

For `try_ip` (client-requested specific IP), there is no retry -- the conflict
propagates immediately and the plugin falls through to range-based allocation.

### Revision Tracking

Each `LeaseRecord` carries a monotonic `revision` field (starting at 1,
incremented on every write). Conflict detection is application-level:
the coordinator reads the existing IP index before writing and compares
ownership. This is *not* based on JetStream's built-in CAS -- the KV put
is unconditional.

---

## Degraded Mode

When NATS becomes unavailable, the cluster enters degraded mode. The behavior
differs by operation type:

| Operation | Behavior |
|---|---|
| **New allocation** (DISCOVER, SOLICIT) | **Blocked.** Returns no response; client retries and may reach a healthy node. |
| **Lease confirmation** (REQUEST) | **Blocked** unless it is a renewal of a known active lease. Known renewals are served locally. |
| **Renew** (DHCPv6) | Same as REQUEST -- allowed for known leases. |
| **Release** | Local release proceeds. NATS coordination skipped. |
| **Decline** | Local probation proceeds. NATS coordination skipped. |
| **Host option lookup** | Returns `Error` outcome. DHCP response is served without host-specific options. |

### Known-Lease Cache

Each node maintains an in-memory cache of active leases it has coordinated.
This cache enables degraded-mode renewals:

- **Populated** on every successful NATS coordination (reserve, lease).
- **Lazy expiry** -- entries are checked against `expires_at` on read.
- **Explicitly removed** on release and decline.
- **Rebuilt** via reconciliation after NATS recovery.

### Post-Outage Reconciliation

When NATS connectivity is restored, a node can call `reconcile()` which:

1. Scans all keys in the `dora_leases` KV bucket.
2. Clears the local known-lease cache.
3. Rebuilds it from all active (`Reserved` / `Leased`) records.
4. Increments `cluster_reconciliations` and `cluster_records_reconciled` metrics.

---

## NATS JetStream Storage Layout

### Leases Bucket (`dora_leases`)

- **History:** 16 revisions per key
- **TTL:** None (application-managed via GC sweep)

Key characters `/` and `:` are sanitized to `_` in all keys.

**Lease record keys:**

| Protocol | Pattern | Example |
|---|---|---|
| DHCPv4 | `v4/{subnet}/client/{client_key}` | `v4/10.0.0.0_24/client/aabb` |
| DHCPv6 | `v6/{subnet}/duid/{duid}/iaid/{iaid}` | `v6/2001_db8___64/duid/00010001aabb/iaid/1` |

**IP index keys** (reverse lookup):

| Protocol | Pattern | Example |
|---|---|---|
| DHCPv4 | `v4/{subnet}/ip/{address}` | `v4/10.0.0.0_24/ip/10.0.0.50` |
| DHCPv6 | `v6/{subnet}/ip/{address}` | `v6/2001_db8___64/ip/2001_db8__100` |

The IP index maps an IP address back to the lease record key that owns it.
This enables conflict detection: before writing a lease, the coordinator
reads the IP index to check if another client already holds that address.

**LeaseRecord payload (JSON):**

```json
{
  "lease_id": "550e8400-e29b-41d4-a716-446655440000",
  "protocol_family": "dhcpv4",
  "subnet": "10.0.0.0/24",
  "ip_address": "10.0.0.50",
  "client_key_v4": "aabbccddeeff",
  "state": "leased",
  "expires_at": "2026-02-27T12:00:00Z",
  "server_id": "node-a",
  "revision": 3,
  "updated_at": "2026-02-27T11:00:00Z"
}
```

**Lease states:**

| State | Meaning | IP index |
|---|---|---|
| `reserved` | Offered, not yet confirmed | Kept |
| `leased` | Confirmed binding | Kept |
| `probated` | Declined / conflicted, IP quarantined | Kept |
| `released` | Client released the lease | Deleted |
| `expired` | GC marked as expired | Deleted |

### Host Options Bucket (`dora_host_options`)

- **History:** 1 (latest value only)
- **TTL:** None

**Key formats (searched in priority order):**

DHCPv4:

| Priority | Pattern | Example |
|---|---|---|
| 1 | `v4/{subnet}/client-id/{client_id}` | `v4/10.0.0.0_24/client-id/aabb` |
| 2 | `v4/client-id/{client_id}` | `v4/client-id/aabb` |
| 3 | `v4/{subnet}/mac/{mac}` | `v4/10.0.0.0_24/mac/aa_bb_cc_dd_ee_ff` |
| 4 | `v4/mac/{mac}` | `v4/mac/aa_bb_cc_dd_ee_ff` |

DHCPv6:

| Priority | Pattern | Example |
|---|---|---|
| 1 | `v6/{subnet}/duid/{duid}/iaid/{iaid}` | `v6/fd00_2___64/duid/0001aabb/iaid/1` |
| 2 | `v6/duid/{duid}/iaid/{iaid}` | `v6/duid/0001aabb/iaid/1` |
| 3 | `v6/{subnet}/duid/{duid}` | `v6/fd00_2___64/duid/0001aabb` |
| 4 | `v6/duid/{duid}` | `v6/duid/0001aabb` |

Lookup stops at the first hit (most specific wins).

### Garbage Collection

A periodic GC sweep runs every `lease_gc_interval_ms` (default 60s):

1. **Orphan index cleanup:** Deletes IP index entries whose referenced lease
   record no longer exists or is inactive/expired.
2. **Expired record transition:** Marks active leases past their `expires_at`
   as `expired` and deletes their IP index entries.

---

## Host Option Overrides

The `nats-host-options` plugin enriches DHCP responses with per-host options
stored in the `dora_host_options` NATS KV bucket. This enables runtime
configuration of PXE boot parameters, TFTP servers, and similar options
without restarting any Dora instance.

### Writing Overrides

Use the `nats` CLI to write host-specific options:

```bash
# Set a boot file for a specific MAC address (global, any subnet)
nats kv put dora_host_options \
  'v4/mac/aa_bb_cc_dd_ee_ff' \
  '{"boot_file": "custom-boot.ipxe", "next_server": "10.0.0.1"}'

# Set options for a specific subnet + client-id combination
nats kv put dora_host_options \
  'v4/192.168.1.0_24/client-id/01aabbccddeeff' \
  '{"boot_file": "pxe-install.ipxe"}'

# DHCPv6: set a boot file URL by DUID
nats kv put dora_host_options \
  'v6/duid/00010001aabbccdd' \
  '{"bootfile_url": "http://boot.example.com/grub.efi"}'
```

### Supported DHCPv4 Payload Keys

| Key | Maps to | DHCP field |
|---|---|---|
| `boot_file` / `bootfile` / `filename` / `bootfile_name` | `fname` header | Boot file name |
| `next_server` / `siaddr` | `siaddr` header | Next server IP |
| `server_name` / `sname` / `tftp_server` | `sname` header | Server hostname |

### Supported DHCPv6 Payload Keys

| Key | Maps to | RFC |
|---|---|---|
| `bootfile_url` / `boot_file_url` | Option 59 | RFC 5970 |
| `bootfile_param` / `boot_file_param` | Option 60 | RFC 5970 |

### Removing Overrides

```bash
nats kv del dora_host_options 'v4/mac/aa_bb_cc_dd_ee_ff'
```

The next DHCP response for that client will fall back to the default options
configured in the Dora YAML config.

---

## Prometheus Metrics

All metrics are exported on the standard `/metrics` endpoint.

### Cluster Coordination (DHCPv4)

| Metric | Type | Description |
|---|---|---|
| `cluster_coordination_state` | Gauge | `1` = NATS connected, `0` = degraded |
| `cluster_allocations_blocked` | Counter | New allocations blocked (NATS down) |
| `cluster_degraded_renewals` | Counter | Renewals served from local cache |
| `cluster_conflicts_detected` | Counter | IP conflicts detected |
| `cluster_conflicts_resolved` | Counter | Conflicts resolved by retry |
| `cluster_reconciliations` | Counter | Post-outage reconciliation runs |
| `cluster_records_reconciled` | Counter | Records rebuilt during reconciliation |
| `cluster_gc_sweeps` | Counter | GC sweep executions |
| `cluster_gc_expired_records` | Counter | Leases marked expired by GC |
| `cluster_gc_orphaned_indexes` | Counter | Orphan IP-index entries cleaned |
| `cluster_gc_errors` | Counter | GC sweep failures |

### Cluster Coordination (DHCPv6)

| Metric | Type | Description |
|---|---|---|
| `cluster_v6_allocations` | Counter | Successful v6 lease allocations |
| `cluster_v6_renewals` | Counter | Successful v6 renewals |
| `cluster_v6_releases` | Counter | v6 releases processed |
| `cluster_v6_declines` | Counter | v6 declines processed |
| `cluster_v6_allocations_blocked` | Counter | v6 allocations blocked (NATS down) |
| `cluster_v6_degraded_renewals` | Counter | v6 degraded-mode renewals |
| `cluster_v6_conflicts` | Counter | v6 coordination conflicts |
| `cluster_v6_invalid_key` | Counter | Requests with missing DUID/IAID |

### Host Option Lookups

| Metric | Type | Description |
|---|---|---|
| `host_option_lookup_hit` | Counter | KV lookup found matching options |
| `host_option_lookup_miss` | Counter | KV lookup found nothing |
| `host_option_lookup_error` | Counter | KV lookup failed |

---

## NixOS Deployment

### Two-Node Cluster with Systemd

Below is a minimal NixOS configuration for a two-node NATS + Dora cluster.
Both nodes should share the same Dora network/range configuration.

```nix
# On each node (adjust IPs and instance-id):
{ pkgs, dora, ... }:
{
  # NATS server with JetStream
  systemd.services.nats = {
    wantedBy = [ "multi-user.target" ];
    serviceConfig.ExecStart = ''
      ${pkgs.nats-server}/bin/nats-server \
        -p 4222 -js \
        --cluster_name dora-js \
        --cluster nats://0.0.0.0:6222 \
        --routes nats://<peer-ip>:6222
    '';
  };

  # Dora DHCP in NATS mode
  systemd.services.dora = {
    wantedBy = [ "multi-user.target" ];
    after = [ "nats.service" ];
    wants = [ "nats.service" ];
    environment = {
      DORA_LOG = "info";
      DORA_ID = "<unique-instance-id>";
    };
    serviceConfig = {
      ExecStart = ''
        ${dora}/bin/dora -c /etc/dora/config.yaml
      '';
      AmbientCapabilities = "CAP_NET_BIND_SERVICE";
    };
  };
}
```

### Verifying the Cluster

```bash
# Check NATS JetStream is ready
nats account info

# List KV buckets (created on first Dora startup)
nats kv ls

# Watch lease activity in real time
nats kv watch dora_leases

# Check Dora metrics
curl -s http://localhost:9300/metrics | grep cluster_
```

### Testing with the Client Matrix

The repository includes a NixOS VM test framework that validates the cluster
against 7 different DHCP clients (dhcpcd, udhcpc, systemd-networkd,
dhcp-loadtest, perfdhcp, dhcpm, dhcping):

```bash
# Run the standalone matrix
nix build .#checks.x86_64-linux.dhcp-client-matrix-standalone -L

# Run the NATS-clustered matrix
nix build .#checks.x86_64-linux.dhcp-client-matrix-nats -L

# Run the dedicated NATS integration test
nix build .#checks.x86_64-linux.dhcp-nats-jetstream-load -L

# Generate a combined report
nix build .#checks.x86_64-linux.dhcp-matrix-report -L
cat result/matrix.md
```

Or use the justfile shortcuts:

```bash
just test-matrix           # both standalone + NATS matrices
just test-nats             # NATS JetStream integration test
just matrix-show           # build and display the combined report
```
