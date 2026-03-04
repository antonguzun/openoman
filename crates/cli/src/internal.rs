use std::{env, net::Ipv4Addr, process::Command as ProcessCommand};

use crate::cli::{FirecrackerNetCommands, InternalCommands};

pub(crate) fn run_internal(command: InternalCommands) -> Result<(), String> {
    match command {
        InternalCommands::FirecrackerNet { command } => run_firecracker_net_internal(command),
    }
}

fn run_firecracker_net_internal(command: FirecrackerNetCommands) -> Result<(), String> {
    match command {
        FirecrackerNetCommands::Setup {
            tap_name,
            host_ip,
            prefix_len,
            allow_all,
        } => firecracker_net_setup(&tap_name, host_ip, prefix_len, allow_all),
        FirecrackerNetCommands::Teardown { tap_name } => firecracker_net_teardown(&tap_name),
    }
}

fn firecracker_net_setup(
    tap_name: &str,
    host_ip: Ipv4Addr,
    prefix_len: u8,
    allow_all: bool,
) -> Result<(), String> {
    validate_tap_name(tap_name)?;
    if prefix_len > 30 {
        return Err("prefix_len must be 30 or smaller".to_string());
    }

    let _ = firecracker_net_teardown(tap_name);
    let mut tuntap_args = vec![
        "tuntap".to_string(),
        "add".to_string(),
        "dev".to_string(),
        tap_name.to_string(),
        "mode".to_string(),
        "tap".to_string(),
    ];
    if let Ok(uid) = env::var("SUDO_UID") {
        tuntap_args.push("user".to_string());
        tuntap_args.push(uid);
    }
    if let Ok(gid) = env::var("SUDO_GID") {
        tuntap_args.push("group".to_string());
        tuntap_args.push(gid);
    }
    run_ip_command(&tuntap_args)?;

    let cidr = format!("{host_ip}/{prefix_len}");
    run_ip_command(&[
        "addr".to_string(),
        "add".to_string(),
        cidr,
        "dev".to_string(),
        tap_name.to_string(),
    ])?;
    run_ip_command(&[
        "link".to_string(),
        "set".to_string(),
        "dev".to_string(),
        tap_name.to_string(),
        "up".to_string(),
    ])?;
    if allow_all {
        configure_firecracker_nat(tap_name, host_ip, prefix_len)?;
    }
    Ok(())
}

fn firecracker_net_teardown(tap_name: &str) -> Result<(), String> {
    validate_tap_name(tap_name)?;
    let subnet = firecracker_tap_subnet_cidr(tap_name).ok();
    let _ = teardown_firecracker_nat(subnet.as_deref());
    let output = ProcessCommand::new("ip")
        .args(["link", "delete", "dev", tap_name])
        .output()
        .map_err(|e| format!("failed to launch ip link delete for {tap_name}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("Cannot find device") || stderr.contains("does not exist") {
        return Ok(());
    }

    Err(format!(
        "failed to delete tap device {tap_name}: {}",
        stderr.trim()
    ))
}

fn validate_tap_name(tap_name: &str) -> Result<(), String> {
    if tap_name.is_empty() || tap_name.len() > 15 {
        return Err("tap_name must be 1-15 characters".to_string());
    }
    if !tap_name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(
            "tap_name may contain only ASCII letters, digits, '.', '-', and '_'".to_string(),
        );
    }
    Ok(())
}

fn configure_firecracker_nat(
    tap_name: &str,
    host_ip: Ipv4Addr,
    prefix_len: u8,
) -> Result<(), String> {
    let subnet = ipv4_network_cidr(host_ip, prefix_len)?;
    run_sysctl_command("net.ipv4.ip_forward=1")?;
    run_iptables_command(&[
        "-A".to_string(),
        "FORWARD".to_string(),
        "-i".to_string(),
        tap_name.to_string(),
        "-s".to_string(),
        subnet.clone(),
        "-j".to_string(),
        "ACCEPT".to_string(),
    ])?;
    run_iptables_command(&[
        "-A".to_string(),
        "FORWARD".to_string(),
        "-d".to_string(),
        subnet.clone(),
        "-m".to_string(),
        "conntrack".to_string(),
        "--ctstate".to_string(),
        "ESTABLISHED,RELATED".to_string(),
        "-j".to_string(),
        "ACCEPT".to_string(),
    ])?;
    run_iptables_command(&[
        "-t".to_string(),
        "nat".to_string(),
        "-A".to_string(),
        "POSTROUTING".to_string(),
        "-s".to_string(),
        subnet,
        "-j".to_string(),
        "MASQUERADE".to_string(),
    ])?;
    Ok(())
}

fn teardown_firecracker_nat(subnet: Option<&str>) -> Result<(), String> {
    let Some(subnet) = subnet else {
        return Ok(());
    };
    let _ = run_iptables_command_allow_missing(&[
        "-D".to_string(),
        "FORWARD".to_string(),
        "-d".to_string(),
        subnet.to_string(),
        "-m".to_string(),
        "conntrack".to_string(),
        "--ctstate".to_string(),
        "ESTABLISHED,RELATED".to_string(),
        "-j".to_string(),
        "ACCEPT".to_string(),
    ]);
    let _ = run_iptables_command_allow_missing(&[
        "-D".to_string(),
        "FORWARD".to_string(),
        "-s".to_string(),
        subnet.to_string(),
        "-j".to_string(),
        "ACCEPT".to_string(),
    ]);
    let _ = run_iptables_command_allow_missing(&[
        "-t".to_string(),
        "nat".to_string(),
        "-D".to_string(),
        "POSTROUTING".to_string(),
        "-s".to_string(),
        subnet.to_string(),
        "-j".to_string(),
        "MASQUERADE".to_string(),
    ]);
    Ok(())
}

fn firecracker_tap_subnet_cidr(tap_name: &str) -> Result<String, String> {
    let output = ProcessCommand::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", tap_name])
        .output()
        .map_err(|e| format!("failed to inspect tap device {tap_name}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not exist") || stderr.contains("Cannot find device") {
            return Err(stderr.trim().to_string());
        }
        return Err(format!(
            "failed to inspect tap device {tap_name}: {}",
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let cidr = stdout
        .split_whitespace()
        .skip_while(|token| *token != "inet")
        .nth(1)
        .ok_or_else(|| format!("tap device {tap_name} has no IPv4 address"))?;
    let (address, prefix_len) = cidr
        .split_once('/')
        .ok_or_else(|| format!("tap device {tap_name} reported invalid IPv4 CIDR: {cidr}"))?;
    let host_ip = address
        .parse::<Ipv4Addr>()
        .map_err(|e| format!("invalid tap host IPv4 {address}: {e}"))?;
    let prefix_len = prefix_len
        .parse::<u8>()
        .map_err(|e| format!("invalid tap prefix length {prefix_len}: {e}"))?;
    ipv4_network_cidr(host_ip, prefix_len)
}

fn ipv4_network_cidr(host_ip: Ipv4Addr, prefix_len: u8) -> Result<String, String> {
    if prefix_len > 32 {
        return Err(format!("invalid IPv4 prefix length {prefix_len}"));
    }
    let mask = if prefix_len == 0 {
        0
    } else {
        u32::MAX << u32::from(32 - prefix_len)
    };
    let network = Ipv4Addr::from(u32::from(host_ip) & mask);
    Ok(format!("{network}/{prefix_len}"))
}

fn run_sysctl_command(setting: &str) -> Result<(), String> {
    let output = ProcessCommand::new("sysctl")
        .args(["-w", setting])
        .output()
        .map_err(|e| format!("failed to launch sysctl -w {setting}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "sysctl -w {setting} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn run_iptables_command(args: &[String]) -> Result<(), String> {
    let output = ProcessCommand::new("iptables")
        .args(args)
        .output()
        .map_err(|e| format!("failed to launch iptables {}: {e}", args.join(" ")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "iptables {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn run_iptables_command_allow_missing(args: &[String]) -> Result<(), String> {
    let output = ProcessCommand::new("iptables")
        .args(args)
        .output()
        .map_err(|e| format!("failed to launch iptables {}: {e}", args.join(" ")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("No chain/target/match by that name")
        || stderr.contains("Bad rule")
        || stderr.contains("No such file or directory")
    {
        return Ok(());
    }
    Err(format!(
        "iptables {} failed: {}",
        args.join(" "),
        stderr.trim()
    ))
}

fn run_ip_command(args: &[String]) -> Result<(), String> {
    let output = ProcessCommand::new("ip")
        .args(args)
        .output()
        .map_err(|e| format!("failed to launch ip {}: {e}", args.join(" ")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "ip {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}
