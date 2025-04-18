# dora (dhcp server)

![](dora.jpg)

`dora` is a DHCP server written in Rust using tokio. It is built on the [`dhcproto`](https://github.com/bluecatengineering/dhcproto) library and `sqlx`. We currently use the sqlite backend, although that could change in the future. The goal of `dora` is to provide a complete, performant, and correct implementation of DHCPv4, and eventually DHCPv6. Dora supports duplicate address detection, ping, binding multiple interfaces, static addresses, client classes, DDNS (**new!**), metrics and leases HTTP API [see example.yaml for all options](./example.yaml).

It is, however, in development and may contain bugs. We hope to build a community around this project. To that end, PRs, issues, and constructive comments are welcome.

You can see all the options available by looking through `example.yaml`. `dora` will parse equivalent JSON or YAML formats of the schema.

If started on non-default dhcp port, it is assumed this is for testing, and dora will unicast any response back rather than following the RFC.

## Features

[see example.yaml for all available options](./example.yaml).

## Build/Run

[To build and run dora in docker see docs/docker.md](./docs/docker.md)

`dora` requires a config file to start. See [example.yaml](./example.yaml) for all available options.

Use `DORA_LOG` env var for adjusting log level and which targets, see [here](https://docs.rs/tracing-subscriber/0.2.20/tracing_subscriber/fmt/index.html#filtering-events-with-environment-variables) for more options.

### Run dora quickly from source

To run dora, bind to the default v4 addr (`0.0.0.0:67`) with a particular config use:

```
cargo run --bin dora -- -c path/to/config.json -d leases.db
```

### Build a dora binary

```
cargo build
```

optional: use `--release` flag for optimized binary without debug symbols

binary will be present in target/{debug,release}/dora

### Building dora for development

You will need `sqlx-cli` to build, as sql queries written in Rust are checked against the database at compile time. [Install sqlx-cli](https://crates.io/crates/sqlx-cli)

From workspace root run:

```
sqlx database create
sqlx migrate run
```

This should create the `em.db` database specified in `.env`, it uses the `DATABASE_URL` env var so make sure that's not in use elsewhere, it can also be passed using `-d/--database-url`.

Use standard cargo subcommands to build (with the `--release` flag for no debug symbols):

```
cargo build
```

and run (by default dora will try to bind to privileged ports, which may require sudo), see the main dora binary [README](bin/README.md) for parameters.

Or run help:

```
cargo run --bin dora -- --help
```

### Cross compiling to ARM

#### Using cross

There is a project called `cross` that does most of the heavy lifting and will build everything in a container, this is the first thing to try. Note that [you will need either docker or podman](https://github.com/cross-rs/cross#dependencies), so we recommend that you install [`docker`](https://docs.docker.com/engine/install/) if you have not yet done so.

```
cargo install cross
cross build --target armv7-unknown-linux-gnueabihf --bin dora --release
```

**Note** Remember to pass `--release` to `cross` if you want an optimized version of the binary

You can compile for the `musl` target also, although it will not have `jemallocator`:

```
cross build --target armv7-unknown-linux-musleabihf --bin dora --release
```

If that works, you should have a `dora` binary in `target/armv7-unknown-linux-gnueabihf/release/dora` or `target/armv7-unknown-linux-musleabihf/release/dora`

#### Not using cross

Firstly, you need the ARM toolchain from rustup:

```
rustup target add armv7-unknown-linux-gnueabihf
```

Notice that `.cargo/config.toml` has an entry for replacing the linker when cross compiling to ARM:

```
[target.armv7-unknown-linux-gnueabihf]
linker = "arm-linux-gnueabihf-gcc"
```

This means `arm-linux-gnueabihf-gcc` must be available on the system and will be used as the linker. Once you have it installed, you can produce an ARMv7 binary using:

```
TARGET_CC=arm-linux-gnueabihf-gcc TARGET_AR=arm-linux-gnueabihf-gcc-ar cargo build --target=armv7-unknown-linux-gnueabihf --bin dora
```

## Dora options & environment vars

[see dora bin readme](bin/README.md)

dora uses the [tracing](https://github.com/tokio-rs/tracing) library for stdout logs.

## Config format

There is a tool included in the workspace called `dora-cfg`, you can run it with:

```
cargo run --bin dora-cfg -- <args>
```

It will pretty-print the internal dora config representation as well as parse the wire format so hex encoded values are human-readable.

[see dora-cfg readme](dora-cfg/README.md)

## HTTP API

By default dora binds to `0.0.0.0:3333`, the following endpoints are defined:

```
/health
/ping
/metrics
/metrics-text
/v1/leases
/config
```

The leases endpoint returns JSON in the format of:

```
❯ curl 0.0.0.0:3333/v1/leases | jq
{
  "networks": {
    "192.168.5.0/24": {
      "ips": [
        {
          "type": "leased",
          "ip": "192.168.5.2",
          "id": "c08fd9962fc1",
          "expires_at_epoch": 1743963741,
          "expires_at_utc": "2025-04-06T18:22:21+00:00"
        },
        {
          "type": "reserved",
          "ip": "192.168.5.100",
          "id": null,
          "match": {
            "options": {
              "values": {
                "60": {
                  "type": "hex",
                  "value": "666f6f626172"
                }
              }
            }
          }
        }
      ]
    }
  }
}
```

## DHCP info

-   [v4 FSM](http://www.tcpipguide.com/free/t_DHCPGeneralOperationandClientFiniteStateMachine.htm)
-   [v4 RFC2131](https://datatracker.ietf.org/doc/html/rfc2131)
-   [v4 RFC2132](https://datatracker.ietf.org/doc/html/rfc2132)
-   [v6 RFC8415](https://datatracker.ietf.org/doc/html/rfc8415)
-   [v4 DHCP basics](https://docs.microsoft.com/en-us/windows-server/troubleshoot/dynamic-host-configuration-protocol-basics)
-   [network sorcery v4](http://www.networksorcery.com/enp/protocol/dhcp.htm)
-   [network sorcery v6](http://www.networksorcery.com/enp/protocol/dhcpv6.htm)

### RFCs implemented in dora

#### v4

-   [v4 RFC1497](https://datatracker.ietf.org/doc/html/rfc1497)
-   [v4 RFC2131](https://datatracker.ietf.org/doc/html/rfc2131)
-   [v4 RFC2132](https://datatracker.ietf.org/doc/html/rfc2132)
-   [v4 RFC3011](https://datatracker.ietf.org/doc/html/rfc3011)
-   [v4 RFC3527](https://datatracker.ietf.org/doc/html/rfc3527)
-   [v4 RFC4578](https://datatracker.ietf.org/doc/html/rfc4578)
-   [v4 RFC4093](https://datatracker.ietf.org/doc/html/rfc4093)
-   [v4 RFC6842](https://datatracker.ietf.org/doc/html/rfc6842)
-   [v4 RFC3046](https://datatracker.ietf.org/doc/html/rfc3046)
-   [v4 RFC5107](https://datatracker.ietf.org/doc/html/rfc5107)
-   [v4 RFC4701](https://www.rfc-editor.org/rfc/rfc4701)
-   [v4 RFC4702](https://www.rfc-editor.org/rfc/rfc4702)
-   [v4 RFC4703](https://www.rfc-editor.org/rfc/rfc4703)

-   see [dhcproto](https://github.com/bluecatengineering/dhcproto) for protocol level support

#### v6

-   [v6 RFC3736](https://www.rfc-editor.org/rfc/rfc3736.html)

## Performance

In synthetic tests with `perfdhcp` I was able to get to around 5000 leases/sec, but `dora` was nowhere near close to consuming available CPU. `dora` keeps no leases in memory at the moment. It relies totally on the database in order to determine which is the next IP to allocate within a range. The db workload is fairly write-heavy, and `sqlite` seems to bottleneck the tokio runtime at a certain point.

We _could_ go much faster by keeping leases in memory and appending to the db like more traditional DHCP implementations, but this is a trade-off for complexity. I've experimented with the bitmap from `roaring-rs` and it seems pretty fast, although we'd need logic to reload the database into memory again on startup and be able to evict entries after lease expiration. Additional complexity we don't care for at the moment. There may be other ways to squeeze more performance out without having to go down this road.

## Troubleshooting/Testing

### Using dhcpm

[dhcpm](https://github.com/leshow/dhcpm) is a tool built in rust that that will mock dhcp requests and is highly useful for testing dhcp in an isolated manner.

### Using perfdhcp

[perfdhcp](https://kea.readthedocs.io/en/kea-2.0.1/man/perfdhcp.8.html) can be used to test dora, include `giaddr`, the subnet select option or the relay agent link selection opt, you can use this as a starting point:

`perfdhcp` is a component of `kea-admin` so you'll need to install it to get the binary:

Ubuntu/Debian:
`sudo apt-get install kea-admin`

```
sudo perfdhcp -4 -N 9900 -L 9903 -r 1 -xi -t 1 -o 118,C0A80001 -R 100 127.0.0.1
```

This will start perfdhcp using dhcpv4, send messages to `127.0.0.1:9900`, listen on port `9903` at a rate of 1/sec, and using 100 different devices. It includes the subnet select opt (118) with `C0A80001` as a hex encoded value of the integer of `192.168.0.1`. `dora` must be listening on `9900` and have a config with ranges to allocate on the `192.168.0.1` network.

### Setting up dora on the PI

See [PI setup](./docs/pi_setup.md)

### Other issues?

If you find a bug, or see something that doesn't look right, please open an issue and let us know. We welcome any and all constructive feedback.

We're still actively working on this. Some of the things we'd like to add in the future include: DDNS updates, stateful DHCPv6, HA & Client classification.
