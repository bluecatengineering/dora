use std::{
    env,
    process::{Child, Command},
};

#[derive(Debug)]
pub(crate) struct DhcpServerEnv {
    daemon: Child,
    db: String,
    netns: String,
    veth_cli: String,
    // veth_srv: String,
    // srv_ip: String,
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
            db: db.to_owned(),
            netns: netns.to_owned(),
            veth_cli: veth_cli.to_owned(),
            // veth_srv: veth_srv.to_owned(),
            // srv_ip: srv_ip.to_owned(),
        }
    }
}

impl Drop for DhcpServerEnv {
    fn drop(&mut self) {
        let db = &self.db;
        stop_dhcp_server(&mut self.daemon);
        remove_test_veth_nics(&self.veth_cli);
        remove_test_net_namespace(&self.netns);
        if let Err(err) = std::fs::remove_file(db) {
            eprintln!("{:?}", err);
        }
        if let Err(err) = std::fs::remove_file(format!("{db}-shm")) {
            eprintln!("{:?}", err);
        }
        if let Err(err) = std::fs::remove_file(format!("{db}-wal")) {
            eprintln!("{:?}", err);
        }
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
    // TODO: remove this eventually
    run_cmd(&format!("ip addr add 192.168.2.99/24 dev {veth_cli}"));
}

fn remove_test_veth_nics(veth_cli: &str) {
    run_cmd_ignore_failure(&format!("ip link del {veth_cli}"));
}

fn start_dhcp_server(config: &str, netns: &str, db: &str) -> Child {
    let workspace_root = env::var("WORKSPACE_ROOT").unwrap_or_else(|_| "..".to_owned());
    let config_path = format!("{workspace_root}/bin/tests/test_configs/{config}");
    let dora_debug = format!(
        "./{workspace_root}/target/debug/dora -d={db} --config-path={config_path} --threads=2 --dora-log=debug --v4-addr=0.0.0.0:9900",
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
