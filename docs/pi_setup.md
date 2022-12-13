# Setting up dora on the Pi

You will need a Pi3 or later with either an onboard WiFi module or an external WiFi dongle. We also assume that you are running Raspbian (tested with 32-bit).

### 0. SSH into the Pi

Ensure everything is up to date with:

```bash
sudo apt update
sudo apt full-upgrade
```

Then ensure you acquire the prerequisites:

```bash
sudo apt-get -y install hostapd bridge-utils iptables gettext libdbus-1-dev libidn11-dev libnetfilter-conntrack-dev nettle-dev netfilter-persistent iptables-persistent
```

Also, if you have not yet installed Rust on your Pi, this can be achieved rather painlessly using [`rustup`](https://rustup.rs)

## Set up the Pi as an access point

You may find [this guide](https://www.raspberrypi.com/documentation/computers/configuration.html#setting-up-a-routed-wireless-access-point) helpful, but here's our TL;DR to set up dora as an access point:

### 1. Configure the host access point daemon

edit `/etc/hostapd/hostapd.conf`. Note that the `ssid` and `wpa_passphrase` that you specify here will be what you need to use to connect to the access point.

```
interface=wlan0
# use `g` for 2.4 GHz and `a` for 5 GHz
hw_mode=g
# must be a channel available on `iw list` with an appropriate frequency for the `hw_mode` you specify
channel=10
# limit the frequencies used to those allowed in the country
ieee80211d=1
# the country code, see https://en.wikipedia.org/wiki/ISO_3166-1_alpha-2#Current_codes
country_code=CA
# 802.11n support
ieee80211n=1
# QoS support, also required for full speed on 802.11n/ac/ax
wmm_enabled=1
# the name of the access point
ssid=PI_AP
# 1=wpa, 2=wep, 3=both
auth_algs=1
# WPA2 only
wpa=2
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP
wpa_passphrase=somepassword
```

### 2. Define the Wireless Interface IP Configuration

edit `/etc/dhcpcd.conf` and append:

```
interface wlan0
# pick some static IP, this is the subnet we'll serve dora on
static ip_address=192.168.5.1/24 
nohook wpa_supplicant
```

### 3. Set up IP forwarding to eth0  

edit `/etc/sysctl.d/99-sysctl.conf` and either ensure the following lines are uncommented or append them:

```
net.ipv4.ip_forward=1
net.ipv6.conf.all.forwarding=1
```

***Note***: You may also find it useful here to create `/etc/sysctl.d/routed-ap.conf` and set its contents to:

```
# Enable IPv4 routing
net.ipv4.ip_forward=1
```

then reboot the Pi to ensure the configuration settings are properly applied:

```bash
sudo reboot
```

once the Pi has rebooted, SSH back in and execute: 

```bash
sudo iptables -t nat -A POSTROUTING -o eth0 -j MASQUERADE
sudo netfilter-persistent save
```

## Set up & run dora/hostapd

1. get yourself a dora ARM binary. See the [README](../README.md#cross-compiling-to-arm) the section "Cross Compiling to ARM". Note that if you choose to build the binary directly on the Pi, you can simply [follow the build instructions](../README.md#buildrun).

1. Run dora, You can see dora's options with `dora --help`, you likely need to edit the config file. `dora`'s config is in a format that's easy to generate programmatically, not with manual editing as the first priority.

A very simple config (IPv4 only) that matches how this guide has configured hostapd might look like:

```yaml
interfaces: 
  - wlan0
networks:
    192.168.5.0/24:
        probation_period: 86400
        server_id: 192.168.5.1
        ranges:
            - start: 192.168.5.2
              end: 192.168.5.250
              config:
                  lease_time:
                      default: 3600
                      min: 1200
                      max: 4800
              options:
                  values:
                      1: # subnet mask (if not specified, comes from `interfaces`)
                          type: ip
                          value: 255.255.255.0
                      3: # router (if not specified, will come from `interfaces`)
                          type: ip_list
                          value:
                              - 192.168.5.1
                      6: # domain name (if running a DNS server like dnsmasq also, use its IP)
                          type: ip_list
                          value:
                              - 8.8.8.8
                      28: # broadcast addr (if not specified, comes from `interfaces`)
                         type: ip
                         value: 192.168.5.255
```

You may wish to save this minimal config to `pi.yaml` to try it out, or see [example.yaml](../example.yaml) for the full set of options. You can also use `dora --help` to see arguments.

Run dora:

After you have saved the above minimal config to `pi.yaml` in the workspace on the Pi, go ahead and [setup the sqlx database](../README.md#buildrun) if you haven't already. You should then be able to run the following (you may also need to substitute your path to the dora binary for `./dora` depending on whether you compiled it directly on the Pi or copied it from elsewhere):

```
sudo DORA_LOG="debug" ./dora -c pi.yaml -d em.db
```

You can delete `rm em.*` to wipe the database and start fresh.

1. Run hostapd

```
sudo hostapd -d /etc/hostapd/hostapd.conf
```

Try connecting to the `PI_AP` wirelessly (using `somepassword` if you have followed this guide precisely), you can check the dora logs to see if DHCP traffic is being received.

## Add to boot

If everything works, it's time to add it all to start on boot

```
sudo systemctl unmask hostapd # this may or may not be necessary
sudo systemctl enable hostapd
sudo reboot
```

We don't have a way to add the dora binary to systemd at the moment, so it must be run manually. You probably want to SSH in to look at the logs anyway. There are a number of ways that you can ensure `dora` will continue to run beyond your SSH session (e.g. using [tmux](https://github.com/tmux/tmux/wiki), so feel free to use your favorite solution.
