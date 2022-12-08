# Setting up dora on the PI

You need a pi3 or later with onboard WIFI module (or an external WIFI dongle) and raspbian (I'm running 32-bit)

1.  SSH into the pi

```
sudo apt-get -y install hostapd bridge-utils iptables gettext libdbus-1-dev libidn11-dev libnetfilter-conntrack-dev nettle-dev netfilter-persistent iptables-persistent
```

## set up the pi as an access point

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
static ip_address=192.168.5.1/24 # <- pick some static IP, this is the subnet we'll serve dora on
nohook wpa_supplicant
denyinterfaces wlan0
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

## Set up & run dora/hostapd

1. get yourself a dora ARM binary. See the [README](../README.md) the section "Cross Compiling to ARM"

1. Run dora, You can see dora's options with `dora --help`, you likely need to edit the config file. `dora`'s config is in a format that's easy to generate programmatically, not with manual editing as the first priority. zzRemember to specify an `interfaces` section in the config.yaml so hostapd and dora don't use the same interface.

A very simple config that matches how this guide has configured hostapd might look like (see [example.yaml](../example.yaml) for the full set of options):

```yaml
interfaces:
    - wlan0
networks:
    192.168.5.0/24:
        authoritative: true
        probation_period: 86400
        ranges:
            - start: 192.168.5.2
              end: 192.168.5.250
              config:
                  lease_time:
                      default: 3600
              options:
                  values:
                      3: # router
                          type: ip_list
                          value:
                              - 192.168.5.1
                      6: # domain name (if running a DNS server like dnsmasq also, use it's IP)
                          type: ip_list
                          value:
                              - 8.8.8.8
```

(use --help to see `dora` arguments)

Run dora:

```
sudo DORA_LOG="debug" ./dora -c example.yaml -d em.db
```

You can delete `rm em.*` to wipe the database and start fresh.

1. Run hostapd

```
sudo hostapd -d /etc/hostapd/hostapd.conf
```

Try connecting to the `PI_AP` wirelessly, you can check the dora logs to see if DHCP traffic is being received.

## Add to boot

If everything works, it's time to add it all to start on boot

```
sudo systemctl unmask hostapd # this may or may not be necessary
sudo systemctl enable hostapd
sudo reboot
```

We don't have a way to add the dora binary to systemd at the moment, so it must be run manually. You probably want to ssh in to look at the logs anyway.
