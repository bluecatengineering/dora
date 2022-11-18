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

## Set up & run dora/hostapd/dnsmasq

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

## Add to boot

If everything works, it's time to add it all to start on boot

```
sudo systemctl unmask hostapd # this may or may not be necessary
sudo systemctl enable hostapd
sudo systemctl enable dnsmasq
sudo reboot
```

We don't have a way to add the dora binary to systemd at the moment, so it must be run manually. You probably want to ssh in to look at the logs anyway.

## (optional) Running dora bound to an interface

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
