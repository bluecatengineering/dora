# dora

`dora` can be run with both cli options or from environment variables, see help for more info:

```
❯ cargo run -- --help

dora-core 0.1.0
dora is a DHCP server written from the ground up in Rust

USAGE:
    dora [OPTIONS]

OPTIONS:
    -c, --config-path <CONFIG_PATH>
            path to dora's config [env: CONFIG_PATH=] [default: /var/lib/dora/config.yaml]

        --channel-size <CHANNEL_SIZE>
            channel size for various mpsc chans [env: CHANNEL_SIZE=] [default: 10000]

    -d <DATABASE_URL>
            Path to the database use "sqlite::memory:" for in mem db ex. "em.db" NOTE: in memory
            sqlite db connection idle timeout is 5 mins [env:
            DATABASE_URL=sqlite:///home/leshow/dev/work/dora/em.db] [default:
            /var/lib/dora/leases.db]

        --dora-id <DORA_ID>
            ID of this instance [env: DORA_ID=] [default: dora_id]

        --external-api <EXTERNAL_API>
            the v6 address to listen on [env: EXTERNAL_API=] [default: [::]:3333]

    -h, --help
            Print help information

        --max-live-msgs <MAX_LIVE_MSGS>
            max live messages before new messages will begin to be dropped [env: MAX_LIVE_MSGS=]
            [default: 1000]

        --thread-name <THREAD_NAME>
            Worker thread name [env: THREAD_NAME=] [default: dora-dhcp-worker]

        --threads <THREADS>
            How many threads are spawned, default is the # of logical CPU cores [env: THREADS=]

        --timeout <TIMEOUT>
            default timeout, dora will respond within this window or drop [env: TIMEOUT=] [default:
            3]

    -V, --version
            Print version information

        --v4-addr <V4_ADDR>
            the v4 address to listen on [env: V4_ADDR=] [default: 0.0.0.0:67]

        --v6-addr <V6_ADDR>
            the v6 address to listen on [env: V6_ADDR=] [default: [::]:547]
```

## Example

Run on non-standard ports:

```
dora -c /path/to/config.yaml --v4-addr 0.0.0.0:9901
```

is equivalent to:

```
V4_ADDR="0.0.0.0:9901" CONFIG_PATH="/path/to/config.yaml" dora
```

Use `DORA_LOG` to control dora's log level. Takes same arguments as `RUST_LOG`
