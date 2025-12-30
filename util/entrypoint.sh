#!/bin/bash

set -e


# Support docker run --init parameter which obsoletes the use of dumb-init,
# but support dumb-init for those that still use it without --init
if [ $$ -eq 1 ]; then
    run="exec /usr/bin/dumb-init --"
else
    run="exec"
fi

# Single argument to command line is interface name
if [ $# -eq 1 -a -n "$1" ]; then
    # skip wait-for-interface behavior if found in path
    if ! which "$1" >/dev/null; then
        # loop until interface is found, or we give up
        NEXT_WAIT_TIME=1
        until [ -e "/sys/class/net/$1" ] || [ $NEXT_WAIT_TIME -eq 4 ]; do
            sleep $(( NEXT_WAIT_TIME++ ))
            echo "Waiting for interface '$1' to become available... ${NEXT_WAIT_TIME}"
        done
        if [ -e "/sys/class/net/$1" ]; then
            IFACE="$1"
        fi
    fi
fi

# No arguments mean all interfaces
if [ -z "$1" ]; then
    IFACE=" "
fi

if [ -n "$IFACE" ]; then
    # Run dora for specified interface or all interfaces

    data_dir="/var/lib/dora"
    if [ ! -d "$data_dir" ]; then
        echo "Please ensure '$data_dir' folder is available."
        echo 'If you just want to keep your configuration in "data/", add -v "$(pwd)/data:/var/lib/dora" to the docker run command line.'
        exit 1
    fi

    dora_conf="$data_dir/config.yaml"
    if [ ! -r "$dora_conf" ]; then
        echo "Please ensure '$dora_conf' exists and is readable."
        exit 1
    fi

    uid=$(stat -c%u "$data_dir")
    gid=$(stat -c%g "$data_dir")
    groupmod -og $gid dhcpd
    usermod -ou $uid dhcpd

    [ -e "$data_dir/em.db" ] || touch "$data_dir/em.db"
    chown dhcpd:dhcpd "$data_dir/em.db"
    if [ -e "$data_dir/em.db~" ]; then
        chown dhcpd:dhcpd "$data_dir/em.db~"
    fi

    container_id=$(grep docker /proc/self/cgroup | sort -n | head -n 1 | cut -d: -f3 | cut -d/ -f3)
    if perl -e '($id,$name)=@ARGV;$short=substr $id,0,length $name;exit 1 if $name ne $short;exit 0' $container_id $HOSTNAME; then
        echo "You must add the 'docker run' option '--net=host' if you want to provide DHCP service to the host network."
    fi

    $run /usr/local/bin/dora
else
    # Run another binary
    $run "$@"
fi
