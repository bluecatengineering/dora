# dora config cli

```
dora-cfg 0.1.0
dora is a DHCP server written from the ground up in Rust

USAGE:
    dora-cfg --path <PATH> --format <FORMAT>

OPTIONS:
    -f, --format <FORMAT>    print the parsed wire format or the dora internal config format
                             [possible values: wire, internal]
    -h, --help               Print help information
    -p, --path <PATH>        path to dora config. We will determine format from extension. If no
                             extension, we will attempt JSON & YAML
    -V, --version            Print version information
```
