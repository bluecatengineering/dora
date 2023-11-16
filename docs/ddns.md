# DDNS

We have added some early support for DDNS. Please report any issues you find.

[4701](https://www.rfc-editor.org/rfc/rfc4701) <- DHCID rdata
[4702](https://www.rfc-editor.org/rfc/rfc4702) <- Client FQDN option section 4 specifies "server behavior" based on fqdn flags
[4703](https://www.rfc-editor.org/rfc/rfc4703) <- specifies the DHCID RR record that must be included in the DNS update

It's worth looking at Kea's DDNS docs [here](https://kea.readthedocs.io/en/kea-2.0.0/arm/ddns.html#overview)

I propose adding the following section to the dora `config.yaml` file. Upon receiving a client FQDN or hostname, we will search the `forward` and `reverse` lists for the longest matching server. See [server selection](https://kea.readthedocs.io/en/kea-2.0.0/arm/ddns.html#dns-server-selection).

```
ddns:
    # send updates. If the ddns section header is defined, enable_updates defaults to true
    enable_updates: true
    # default false. whether to override the client update FQDN flags
    override_client_updates: false
    # default false. whether to override the no update FQDN flags
    override_no_updates: false
    # list of forward DNS servers
    forward:
       - name: "example.com"
         key: "key_foo" # optional, must match key name in tsig_keys
         ip: 192.168.3.111
    # reverse servers list
    reverse:
       - name: "168.192.in-addr.arpa."
         key: "key_foo" # optional
         ip: 192.168.3.111
    # map of tsig keys. DNS servers reference these by name
    tsig_keys:
        key_foo:
          algorithm: "hmac-sha1"
          data: "<keydata>"
```

If a hostname option is received, it is concatenated with a configured option 15 (domain name) to produce a fqdn, this fqdn is used for the DNS update. Not included in this draft is any other manipulation of the hostname option.

There are a few config values that can change the behavior of the update. These options are similar to what is available in kea:

`enable_updates`: should we process the client FQDN option? true/false
`override_client_updates`: the client FQDN flag can have a flag telling the server that it wants to do the DNS update, setting this to true will _override_ that behavior and send back the relevant 'o' flag set to true. (see here: https://www.rfc-editor.org/rfc/rfc4702.html#section-4)
`override_no_updates`: client FQDN flags can have a 'no update' flag set, if `override_no_updates` is true, then we will do the update anyway and set the override flag on response.

The logic for client FQDN flag handling is largely in the `handle_flags` function, and was translated from [Keas flag handling](https://github.com/isc-projects/kea/blob/9c76b9a9e55b49ea407531b64783f6ec12546f42/src/lib/dhcpsrv/d2_client_mgr.cc#L115)

As for the content of the DNS updates themselves, here is an example of a forward update created by trust-dns-client

![fwd_update](https://user-images.githubusercontent.com/1128302/210460131-97bcf7f1-09aa-4c82-807f-7d5eb19542d3.png)

The DHCID RR is created in accordance with [4701](https://www.rfc-editor.org/rfc/rfc4701#section-3.5).

Here's a reverse update:

![rev_ip](https://user-images.githubusercontent.com/1128302/210626264-c7a1ddbb-1ecd-43a7-ac54-0278a37de3cd.png)
