use std::{
    env,
    process::{Child, Command},
};

pub const TEST_DORA_NETNS: &str = "dora_test";
pub const TEST_NIC_CLI: &str = "dhcpcli";
pub const TEST_NIC_SRV: &str = "dhcpsrv";

pub const TEST_DHCP_SRV_IP: &str = "192.168.2.1";

const DNSMASQ_OPTS: &str = r#"
--log-dhcp
--keep-in-foreground
--no-daemon
--conf-file=/dev/null
--dhcp-leasefile=/tmp/mozim_test_dhcpd_lease
--no-hosts
--dhcp-host=foo1,192.0.2.99
--dhcp-host=00:11:22:33:44:55,192.0.2.51
--dhcp-option=option:dns-server,8.8.8.8,1.1.1.1
--dhcp-option=option:mtu,1492
--dhcp-option=option:domain-name,example.com
--dhcp-option=option:ntp-server,192.0.2.1
--keep-in-foreground
--bind-interfaces
--except-interface=lo
--clear-on-reload
--listen-address=192.0.2.1
--dhcp-range=192.0.2.2,192.0.2.50,60 --no-ping
"#;

#[derive(Debug)]
pub(crate) struct DhcpServerEnv {
    daemon: Child,
    config_filename: String,
    db: String,
    netns: String,
    veth_cli: String,
    veth_srv: String,
    srv_ip: String,
}

impl DhcpServerEnv {
    pub(crate) fn start(
        config: &str,
        db: &str,
        netns: &str,
        veth_cli: &str,
        veth_srv: &str,
        srv_ip: &str,
    ) -> Self {
        create_test_net_namespace(netns);
        create_test_veth_nics(netns, srv_ip, veth_cli, veth_srv);
        Self {
            daemon: start_dhcp_server(config, netns, db),
            config_filename: config.to_owned(),
            db: db.to_owned(),
            netns: netns.to_owned(),
            veth_cli: veth_cli.to_owned(),
            veth_srv: veth_srv.to_owned(),
            srv_ip: srv_ip.to_owned(),
        }
    }
}

impl Drop for DhcpServerEnv {
    fn drop(&mut self) {
        let db = &self.db;
        stop_dhcp_server(&mut self.daemon);
        remove_test_veth_nics(&self.veth_cli);
        remove_test_net_namespace(&self.netns);
        std::fs::remove_file(db);
        std::fs::remove_file(format!("{db}-shm"));
        std::fs::remove_file(format!("{db}-wal"));
    }
}

fn create_test_net_namespace(netns: &str) {
    run_cmd(&format!("ip netns add {netns}"));
}

fn remove_test_net_namespace(netns: &str) {
    run_cmd_ignore_failure(&format!("ip netns del {netns}"));
}

fn create_test_veth_nics(netns: &str, srv_ip: &str, veth_cli: &str, veth_srv: &str) {
    run_cmd(&format!(
        "ip link add {veth_cli} type veth peer name {veth_srv}",
    ));
    run_cmd(&format!("ip link set {veth_cli} up"));
    run_cmd(&format!("ip link set {veth_srv} netns {netns}",));
    run_cmd(&format!("ip netns exec {netns} ip link set {veth_srv} up",));
    run_cmd(&format!(
        "ip netns exec {netns} ip addr add {srv_ip}/24 dev {veth_srv}",
    ));
}

fn remove_test_veth_nics(veth_cli: &str) {
    run_cmd_ignore_failure(&format!("ip link del {veth_cli}"));
}

fn start_dhcp_server(config: &str, netns: &str, db: &str) -> Child {
    let workspace_root = env::var("WORKSPACE_ROOT").unwrap_or_else(|_| "..".to_owned());
    let config_path = format!("{workspace_root}/bin/tests/test_configs/{config}");
    let dora_debug = format!(
        "./{workspace_root}/target/debug/dora -d={db} --config-path={config_path} --threads=2 --dora-log=debug",
    );
    let cmd = format!("ip netns exec {netns} {dora_debug}");

    let cmds: Vec<&str> = cmd.split(' ').collect();
    let mut child = Command::new(cmds[0])
        .args(&cmds[1..])
        .spawn()
        .expect("Failed to start DHCP server");
    std::thread::sleep(std::time::Duration::from_secs(1));
    if let Ok(Some(ret)) = child.try_wait() {
        panic!("Failed to start DHCP server {:?}", ret);
    }
    child
}

fn stop_dhcp_server(daemon: &mut Child) {
    daemon.kill().expect("Failed to stop DHCP server")
}

fn run_cmd(cmd: &str) -> String {
    let cmds: Vec<&str> = cmd.split(' ').collect();
    let output = Command::new(cmds[0])
        .args(&cmds[1..])
        .output()
        .unwrap_or_else(|_| panic!("failed to execute command {}", cmd));
    if !output.status.success() {
        panic!("{}", String::from_utf8_lossy(&output.stderr));
    }

    String::from_utf8(output.stdout).expect("Failed to convert file command output to String")
}

fn run_cmd_ignore_failure(cmd: &str) -> String {
    let cmds: Vec<&str> = cmd.split(' ').collect();
    match Command::new(cmds[0]).args(&cmds[1..]).output() {
        Ok(o) => String::from_utf8(o.stdout).unwrap_or_default(),
        Err(e) => {
            eprintln!("Failed to execute command {}: {}", cmd, e);
            "".to_string()
        }
    }
}
