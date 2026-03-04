use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
};

use serde::Serialize;

use super::super::{AttemptSpec, ExecutionError, FirecrackerBackendConfig};
use super::network::NetworkLease;

pub(super) struct FirecrackerVmLauncher<'a> {
    firecracker: &'a FirecrackerBackendConfig,
}

impl<'a> FirecrackerVmLauncher<'a> {
    pub(super) fn new(firecracker: &'a FirecrackerBackendConfig) -> Self {
        Self { firecracker }
    }

    pub(super) fn write_firecracker_config(
        &self,
        run_dir: &Path,
        rootfs_image_path: &Path,
        runtime_image_path: &Path,
        spec: &AttemptSpec,
        network_lease: Option<&NetworkLease>,
    ) -> Result<std::path::PathBuf, ExecutionError> {
        let config_path = run_dir.join("firecracker-config.json");
        let config = FirecrackerConfigFile {
            boot_source: BootSourceConfig {
                kernel_image_path: self.firecracker.kernel_image_path.display().to_string(),
                boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/sbin/openoman-init".to_string(),
            },
            drives: vec![
                DriveConfig {
                    drive_id: "rootfs".to_string(),
                    path_on_host: rootfs_image_path.display().to_string(),
                    is_root_device: true,
                    is_read_only: false,
                },
                DriveConfig {
                    drive_id: "runtime".to_string(),
                    path_on_host: runtime_image_path.display().to_string(),
                    is_root_device: false,
                    is_read_only: false,
                },
            ],
            machine_config: MachineConfig {
                vcpu_count: spec.limits.vcpu_count.max(1),
                mem_size_mib: spec.limits.memory_mib.max(128),
                smt: false,
            },
            network_interfaces: network_lease
                .map(|lease| {
                    vec![NetworkInterfaceConfig {
                        iface_id: lease.guest_iface.clone(),
                        host_dev_name: lease.tap_name.clone(),
                        guest_mac: lease.guest_mac.clone(),
                    }]
                })
                .unwrap_or_default(),
            entropy: EntropyDeviceConfig {},
        };

        fs::write(
            &config_path,
            serde_json::to_vec_pretty(&config).map_err(|err| {
                ExecutionError::InvalidConfig(format!(
                    "failed to serialize firecracker config: {err}"
                ))
            })?,
        )?;
        Ok(config_path)
    }

    pub(super) fn spawn_firecracker_process(
        &self,
        handle_id: u64,
        job_id: &str,
        prepared_image_path: &Path,
        config_path: &Path,
        serial_log_path: &Path,
        vmm_log_path: &Path,
    ) -> Result<Child, ExecutionError> {
        let serial_log_file = fs::File::create(serial_log_path)?;
        let firecracker_bin = resolve_command_path(&self.firecracker.firecracker_bin)?;

        Command::new(firecracker_bin)
            .arg("--no-api")
            .arg("--id")
            .arg(format!("openoman-{}-{handle_id}", job_id))
            .arg("--log-path")
            .arg(vmm_log_path)
            .arg("--level")
            .arg("Info")
            .arg("--config-file")
            .arg(config_path)
            .stdout(Stdio::from(serial_log_file.try_clone()?))
            .stderr(Stdio::from(serial_log_file))
            .env("OPENOMAN_FAKE_RUNTIME_IMAGE", prepared_image_path)
            .env(
                "OPENOMAN_FAKE_RUNTIME_ROOTFS",
                &self.firecracker.rootfs_image_path,
            )
            .spawn()
            .map_err(ExecutionError::Io)
    }
}

#[derive(Debug, Serialize)]
struct FirecrackerConfigFile {
    #[serde(rename = "boot-source")]
    boot_source: BootSourceConfig,
    drives: Vec<DriveConfig>,
    #[serde(rename = "machine-config")]
    machine_config: MachineConfig,
    #[serde(rename = "network-interfaces", skip_serializing_if = "Vec::is_empty")]
    network_interfaces: Vec<NetworkInterfaceConfig>,
    entropy: EntropyDeviceConfig,
}

#[derive(Debug, Serialize)]
struct BootSourceConfig {
    kernel_image_path: String,
    boot_args: String,
}

#[derive(Debug, Serialize)]
struct DriveConfig {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Debug, Serialize)]
struct MachineConfig {
    vcpu_count: u8,
    mem_size_mib: u32,
    smt: bool,
}

#[derive(Debug, Serialize)]
struct NetworkInterfaceConfig {
    iface_id: String,
    host_dev_name: String,
    guest_mac: String,
}

#[derive(Debug, Serialize)]
struct EntropyDeviceConfig {}

fn resolve_command_path(program: &str) -> Result<std::path::PathBuf, ExecutionError> {
    let program_path = Path::new(program);
    if program_path.is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        if program_path.exists() {
            return Ok(program_path.to_path_buf());
        }
        return Err(ExecutionError::MissingDependency(format!(
            "required command path does not exist: {program}"
        )));
    }

    let path_var = std::env::var_os("PATH").ok_or_else(|| {
        ExecutionError::MissingDependency(format!("PATH is not set while resolving {program}"))
    })?;

    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(ExecutionError::MissingDependency(format!(
        "required command not found in PATH: {program}"
    )))
}
