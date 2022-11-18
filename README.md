# dora (dhcp server)

![](dora.jpg)

[[_TOC_]]

## Intro

`dora` is a DHCP server written in Rust using tokio. It is built on the `dhcproto` library and `sqlx`. We currently use the sqlite backend, although that could change in the future. The goal of `dora` is to provide a complete DHCP implementation, first for IPv4, but also IPv6. It supports things like duplicate address detection, ping, binding multiple interfaces, static addresses, etc.

You can see all the options available by looking through `example.yaml`.

If started on default dhcp port, or with no port provided, `dora` will try to observe the correct rules for when to broadcast vs unicast as defined by the RFC. If started on a port that's non-default then dora will just unicast to whatever the source addr/port was.

## Features

A non-exhaustive list of features, and their location in the project. Developers can use this as a starting point to explore dora's implementation.

| Feature                                       | Description                                                                                        |
| --------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| [Leases (plugin)](plugins/leases)             | Handles assignment of dynamic IPs                                                                  |
| [Static (plugin)](plugins/static_addr)        | Handles assignment of static IPs                                                                   |
| [Message type (plugin)](plugins/message_type) | Sets up preliminary return message type given received message type (other plugins can alter this) |
| [Ip Manager](libs/ip_manager)                 | IP acquisition & storage                                                                           |

## Dev environment setup

[See corten's README for dev setup](https://gitlab.bluecatlabs.net/dns-edge/development/corten-dns#dev-environment-setup)

## Build/Run

### Host

You will need `sqlx-cli` to build, as sql queries written in Rust are checked against the database at compile time. [Install sqlx-cli](https://crates.io/crates/sqlx-cli)

From workspace root run:

```
sqlx database create
sqlx migrate run
```

This should create the `em.db` database specified in `.env`, it uses the `DATABASE_URL` env var so make sure that's not in use elsewhere, it can also be passed using `-d/--database-url`.

Use standard cargo subcommands to build:

```
cargo build
```

and run (by default dora will try to bind to privileged ports, which may require sudo):

```
cargo run --bin dora
```

For HELP, run:

```
cargo run --bin dora -- --help
```

Use `DORA_LOG` env var for adjusting log level and which targets, see [here](https://docs.rs/tracing-subscriber/0.2.20/tracing_subscriber/fmt/index.html#filtering-events-with-environment-variables) for more options.

When running tests on the host machine, be sure to exclude the component tests with:

```
cargo test --exclude component --workspace
```

Or, simply run `make test`, this will test in docker.

### Docker

You can build dora in a docker container with:

```
make build
```

format/clippy can also be run within docker with `make fmt` or `make clippy`. Build in release with `RELEASE=1` and run with the `run` target.

### Cross compiling to ARM

## Using cross

There is a project called `cross` that does most of the heavy lifting and will build everything in a docker container, this is the first thing to try.

```
cargo install cross
cross build --target armv7-unknown-linux-gnueabihf --bin dora --release
```

**Note** Remember to pass `--release` to `cross` if you want an optimized version of the binary

The `musl` target also works, although it will not have `jemallocator`:

```
cross build --target armv7-unknown-linux-musleabihf --bin dora
```

If that works, you should have a `dora` binary in `target/armv7-unknown-linux-gnueabihf/debug/dora` or `target/armv7-unknown-linux-musleabihf/debug/dora`

## Not using cross

Firstly, you need the ARM toolchain from rustup:

```
rustup target add armv7-unknown-linux-gnueabihf
```

Notice that `.cargo/config.toml` has an entry for replacing the linker when cross compiling to ARM:

```
[target.armv7-unknown-linux-gnueabihf]
linker = "arm-linux-gnueabihf-gcc"
```

This means `arm-linux gnueabihf-gcc` must be available on the system and will be used as the linker. Once you have it installed, you can use:

```
TARGET_CC=arm-linux-gnueabihf-gcc TARGET_AR=arm-linux-gnueabihf-gcc-ar cargo build --target=armv7-unknown-linux-gnueabihf --bin dora
```

To produce an ARMv7 binary. I have not tested this on an actual system, but it appears to compile & link.

## Dora options & environment vars

[see dora bin readme](bin/README.md)

## Config format

There is a tool included in the workspace called `dora-cfg`, you can run it with:

```
cargo run --bin dora-cfg -- <args>
```

It will pretty-print the internal dora config representation as well as parse the wire format so hex encoded values are human-readable.

[see dora-cfg readme](dora-cfg/README.md)

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

-   [v4 RFC2131](https://datatracker.ietf.org/doc/html/rfc2131)
-   [v4 RFC2132](https://datatracker.ietf.org/doc/html/rfc2132)
-   [v4 RFC3011](https://datatracker.ietf.org/doc/html/rfc3011)
-   [v4 RFC3527](https://datatracker.ietf.org/doc/html/rfc3527)
-   [v4 RFC4578](https://datatracker.ietf.org/doc/html/rfc4578)
-   [v4 RFC6842](https://datatracker.ietf.org/doc/html/rfc6842)
-   [v4 RFC3046](https://datatracker.ietf.org/doc/html/rfc3046)
-   see [dhcproto](https://github.com/bluecatengineering/dhcproto) for protocol level support, this list is dora only

#### v6

-   [v6 RFC3736](https://www.rfc-editor.org/rfc/rfc3736.html)

## Performance

Dora keeps almost nothing in memory, it relies totally on the database in order to determine which is the next IP to allocate within a range. `UPDATE`s and `INSERT`s happen on discover & request responses, as such the db workload is write-heavy. We also use `sqlite` as our default database backend, which has not-great performance for write heavy workloads.

We _could_ go much faster by keeping things in memory and writing to db after the fact, but this is a trade-off for complexity. Options for speedup:

-   Pre-`INSERT` IPs for each range when config changes so at most we only do a single `UPDATE`
-   keep IP acquisition in memory (`optimize_alloc` branch has some of this). Requires us to keep in-memory structures in sync with database though.
-   Use an alternate db backend like postgres

The are micro-optimizations that could be done in the codebase to tune how much we allocate per-request also, but the above suggestions will have a much higher impact on performance.

## Troubleshooting/Testing

### sqlx is giving me issues

If `sqlx` continues to cause problems, perhaps we can consider changing to `rusqlite` and an async connection pooling mechanism like `deadpool`, [see here](https://crates.io/crates/deadpool-sqlite/0.2.0)

### Using perfdhcp

[perfdhcp](https://kea.readthedocs.io/en/kea-2.0.1/man/perfdhcp.8.html) can be used to test dora, but you must include `giaddr`, the subnet select option or the relay agent link selection opt, you can use this as a starting point:

```
sudo perfdhcp -4 -N 9901 -L 9903 -r 1 -xi -t 1 -o 118,C0A80001 -R 100 127.0.0.1
```

This will start perfdhcp using dhcpv4, send messages to `127.0.0.1:9901`, listen on port `9903` at a rate of 1/sec, and using 100 different devices. It includes the subnet select opt (118) with `C0A80001` as a hex encoded value of the integer of `192.168.0.1`.

### Setting up dora on the PI

You need a pi3 or later with onboard WIFI module (or an external WIFI dongle) and raspbian (I'm running 32-bit)

1.  SSH into the pi

```
sudo apt-get -y install hostapd bridge-utils iptables gettext libdbus-1-dev libidn11-dev libnetfilter-conntrack-dev nettle-dev netfilter-persistent iptables-persistent
```

#### set up the pi as an access point

I followed the guide [here](https://www.raspberrypi.com/documentation/computers/configuration.html#setting-up-a-routed-wireless-access-point)

I have tried to reproduce my steps below:

1. edit /etc/hostapd/hostapd.conf

```
interface=wlan0
hw_mode=g
# must be a channel available on `iw list`
channel=10
# limit the frequencies used to those allowed in the country
ieee80211d=1
# the country code
country_code=EN
# 802.11n support
ieee80211n=1
# QoS support, also required for full speed on 802.11n/ac/ax
wmm_enabled=1
# the name of the AP
ssid=PI_AP
# 1=wpa, 2=wep, 3=both
auth_algs=1
# WPA2 only
wpa=2
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP
wpa_passphrase=somepassword
```

1. edit /etc/dhcpcd.conf

```
interface wlan0
static ip_address=192.168.5.1/24 # pick some static IP
nohook wpa_supplicant
denyinterfaces wlan0
```

1. clone and build dnsmasq

```
git clone git://thekelleys.org.uk/dnsmasq.git
cd dnsmasq
make all-i18n
```

1. set up IP forwarding to eth0

`sudo nvim /etc/sysctl.d/99-sysctl.conf`:

```
net.ipv4.ip_forward=1
net.ipv6.conf.all.forwarding=1
```

```
sudo iptables -t nat -A POSTROUTING -o eth0 -j MASQUERADE
sudo netfilter-persistent save
```

1. reboot

```
sudo reboot
```

#### Set up & run dora/hostapd/dnsmasq

1. get yourself a dora ARM binary. Contact @ecameron on slack or [see cross compiling](#cross-compiling-to-arm)

1. Run dora, You can see dora's options with `dora --help`, you may need to edit the config file. `dora`'s config is in a format that's easy to generate programmatically, not with manual editing as a priority. Make sure the broadcast address, router, and subnet all match what is configured for the wireless interface.

(use --help to see dora opts)

```
sudo DORA_LOG="debug" ./dora -c example.yaml --v4-addr 0.0.0.0:9901 -d em.db
```

You can delete `rm em.*` to wipe the database and start fresh.

1. Run hostapd

```
sudo hostapd -d /etc/hostapd/hostapd.conf
```

1. run dnsamsq

```
sudo ./dnsmasq/src/dnsmasq -d --dhcp-relay=192.168.5.1,127.0.0.1#9901,wlan0
```

Try connecting to the `PI_AP` wirelessly, you can check the dora logs to see if DHCP traffic is being received.

#### Add to boot

If everything works, it's time to add it all to start on boot

```
sudo systemctl unmask hostapd # this may or may not be necessary
sudo systemctl enable hostapd
sudo systemctl enable dnsmasq
sudo reboot
```

We don't have a way to add the dora binary to systemd at the moment, so it must be run manually. You probably want to ssh in to look at the logs anyway.

#### (optional) Running dora bound to an interface

You can skip the `dnsmasq` step and run dora directly bound to `wlan0`, if you include:

```
interface: wlan0
```

in `example.yaml` (the dora config file), and start dora:

```
sudo DORA_LOG="debug" ./dora -c example.yaml -d em.db
```

If no `--v4-addr` is specified, then dora uses `0.0.0.0:67` the default DHCP ports. With this config, dora will respond to broadcast traffic on the interface specified.

**CAVEATS** You can only bind one interface, and only the first block in the `networks` map will be used. This may change in the future, this is limited initial support.
