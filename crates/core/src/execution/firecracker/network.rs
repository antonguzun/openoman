use std::{
    fs,
    io::{self, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream},
    path::Path,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    thread::JoinHandle,
};

use super::super::{
    AttemptSpec, ExecutionError, ExecutionHandle, FirecrackerBackendConfig,
    FirecrackerNetworkPrivilegeMode, FirecrackerNetworkingMode,
};
use super::{FIRECRACKER_NET_HELPER_SUBCOMMAND, PROXY_REQUEST_LIMIT_BYTES};

#[derive(Debug, Clone)]
pub(super) struct NetworkLease {
    pub(super) tap_name: String,
    pub(super) guest_iface: String,
    pub(super) host_ip: Ipv4Addr,
    pub(super) guest_ip: Ipv4Addr,
    pub(super) prefix_len: u8,
    pub(super) guest_mac: String,
    pub(super) proxy_port: u16,
}

impl NetworkLease {
    pub(super) fn guest_ip_cidr(&self) -> String {
        format!("{}/{}", self.guest_ip, self.prefix_len)
    }

    pub(super) fn host_proxy_url(&self) -> String {
        format!("http://{}:{}", self.host_ip, self.proxy_port)
    }
}

#[derive(Debug)]
pub(super) struct NetworkProxyHandle {
    pub(super) bind_addr: SocketAddrV4,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl NetworkProxyHandle {
    pub(super) fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.bind_addr);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

pub(super) struct FirecrackerNetworkController<'a> {
    firecracker: &'a FirecrackerBackendConfig,
}

impl<'a> FirecrackerNetworkController<'a> {
    pub(super) fn new(firecracker: &'a FirecrackerBackendConfig) -> Self {
        Self { firecracker }
    }

    pub(super) fn configure_networking(
        &self,
        handle: &ExecutionHandle,
        spec: &AttemptSpec,
        log_path: &Path,
    ) -> Result<(Option<NetworkLease>, Option<NetworkProxyHandle>), ExecutionError> {
        if self.firecracker.networking.mode != FirecrackerNetworkingMode::HostProxy {
            return Ok((None, None));
        }
        if spec.agent.egress_allowed_domains.is_empty() {
            return Err(ExecutionError::InvalidConfig(
                "agent.egress_allowed_domains must include at least one domain when sandbox.firecracker.networking.mode = host-proxy"
                    .to_string(),
            ));
        }

        let lease = allocate_network_lease(
            &self.firecracker.networking.subnet_cidr,
            handle.id,
            &self.firecracker.networking.tap_name_prefix,
            self.firecracker.networking.proxy_port,
        )?;
        self.append_network_log_line(
            log_path,
            &format!(
                "network setup requested: tap={} host_ip={}/{} guest_ip={}/{} proxy_port={} allowlist={}",
                lease.tap_name,
                lease.host_ip,
                lease.prefix_len,
                lease.guest_ip,
                lease.prefix_len,
                lease.proxy_port,
                spec.agent.egress_allowed_domains.join(",")
            ),
        )?;
        self.run_network_helper_command(
            "setup",
            &[
                ("--tap-name", lease.tap_name.clone()),
                ("--host-ip", lease.host_ip.to_string()),
                ("--prefix-len", lease.prefix_len.to_string()),
                (
                    "--allow-all",
                    match host_proxy_egress_policy(&spec.agent.egress_allowed_domains) {
                        HostProxyEgressPolicy::AllowAllDebug => "true".to_string(),
                        HostProxyEgressPolicy::Restricted(_) => "false".to_string(),
                    },
                ),
            ],
            log_path,
        )?;
        match self.start_network_proxy(&lease, &spec.agent.egress_allowed_domains, log_path) {
            Ok(proxy) => Ok((Some(lease), Some(proxy))),
            Err(err) => {
                let _ = self.teardown_network_lease(&lease, log_path);
                Err(err)
            }
        }
    }

    pub(super) fn append_network_log_line(
        &self,
        log_path: &Path,
        message: &str,
    ) -> Result<(), ExecutionError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        writeln!(file, "{message}")?;
        Ok(())
    }

    pub(super) fn start_network_proxy(
        &self,
        lease: &NetworkLease,
        allowed_domains: &[String],
        log_path: &Path,
    ) -> Result<NetworkProxyHandle, ExecutionError> {
        let requested_bind_addr = SocketAddrV4::new(lease.host_ip, lease.proxy_port);
        let listener = TcpListener::bind(requested_bind_addr)?;
        let bind_addr = match listener.local_addr()? {
            std::net::SocketAddr::V4(addr) => addr,
            std::net::SocketAddr::V6(_) => {
                return Err(ExecutionError::RunnerState(
                    "host proxy unexpectedly bound to an IPv6 address".to_string(),
                ))
            }
        };
        listener.set_nonblocking(true)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let allowed_domains = Arc::new(allowed_domains.to_vec());
        let log_file = Arc::new(Mutex::new(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)?,
        ));
        write_shared_log(&log_file, &format!("host proxy listening on {bind_addr}"));
        let shutdown_for_thread = Arc::clone(&shutdown);
        let log_for_thread = Arc::clone(&log_file);
        let allowed_for_thread = Arc::clone(&allowed_domains);
        let accept_thread = thread::spawn(move || {
            loop {
                if shutdown_for_thread.load(Ordering::SeqCst) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, peer_addr)) => {
                        let log_for_client = Arc::clone(&log_for_thread);
                        let allowed_for_client = Arc::clone(&allowed_for_thread);
                        thread::spawn(move || {
                            handle_connect_proxy_client(
                                stream,
                                peer_addr.to_string(),
                                allowed_for_client,
                                log_for_client,
                            );
                        });
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(25));
                    }
                    Err(err) => {
                        write_shared_log(&log_for_thread, &format!("proxy accept failed: {err}"));
                        thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
            write_shared_log(&log_for_thread, "host proxy stopped");
        });
        Ok(NetworkProxyHandle {
            bind_addr,
            shutdown,
            accept_thread: Some(accept_thread),
        })
    }

    pub(super) fn teardown_network_lease(
        &self,
        lease: &NetworkLease,
        log_path: &Path,
    ) -> Result<(), ExecutionError> {
        self.append_network_log_line(
            log_path,
            &format!("tearing down tap device {}", lease.tap_name),
        )?;
        self.run_network_helper_command(
            "teardown",
            &[("--tap-name", lease.tap_name.clone())],
            log_path,
        )
    }

    fn run_network_helper_command(
        &self,
        action: &str,
        args: &[(&str, String)],
        log_path: &Path,
    ) -> Result<(), ExecutionError> {
        let current_exe = std::env::current_exe().map_err(ExecutionError::Io)?;
        let mut command = match self.firecracker.networking.privilege_mode {
            FirecrackerNetworkPrivilegeMode::Sudo => {
                let mut command = Command::new("sudo");
                command.arg("-n").arg(&current_exe);
                command
            }
            FirecrackerNetworkPrivilegeMode::Direct => Command::new(&current_exe),
        };
        command.args(FIRECRACKER_NET_HELPER_SUBCOMMAND).arg(action);
        let mut display_args = Vec::new();
        if self.firecracker.networking.privilege_mode == FirecrackerNetworkPrivilegeMode::Sudo {
            display_args.push("-n".to_string());
            display_args.push(current_exe.display().to_string());
        }
        display_args.extend(
            FIRECRACKER_NET_HELPER_SUBCOMMAND
                .iter()
                .map(|value| (*value).to_string()),
        );
        display_args.push(action.to_string());
        for (flag, value) in args {
            command.arg(flag).arg(value);
            display_args.push((*flag).to_string());
            display_args.push(value.clone());
        }

        let helper_program = match self.firecracker.networking.privilege_mode {
            FirecrackerNetworkPrivilegeMode::Sudo => "sudo".to_string(),
            FirecrackerNetworkPrivilegeMode::Direct => current_exe.display().to_string(),
        };
        self.append_network_log_line(
            log_path,
            &format!(
                "running network helper: {} {}",
                helper_program,
                display_args.join(" ")
            ),
        )?;

        let output = command.output()?;
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if !stdout.is_empty() {
                self.append_network_log_line(log_path, &format!("helper stdout: {stdout}"))?;
            }
            if !stderr.is_empty() {
                self.append_network_log_line(log_path, &format!("helper stderr: {stderr}"))?;
            }
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        self.append_network_log_line(
            log_path,
            &format!(
                "network helper failed: exit={:?} stderr={stderr}",
                output.status.code()
            ),
        )?;
        Err(ExecutionError::CommandFailed {
            program: match self.firecracker.networking.privilege_mode {
                FirecrackerNetworkPrivilegeMode::Sudo => "sudo".to_string(),
                FirecrackerNetworkPrivilegeMode::Direct => current_exe.display().to_string(),
            },
            args: display_args,
            stderr,
        })
    }
}

type SharedLog = Arc<Mutex<fs::File>>;

const TLS_HANDSHAKE_CONTENT_TYPE: u8 = 22;
const TLS_CLIENT_HELLO_HANDSHAKE_TYPE: u8 = 1;
const TLS_SUPPORTED_VERSIONS_EXTENSION: u16 = 0x002b;
const TLS_MIN_ALLOWED_VERSION: u16 = 0x0303;
const TLS_CLIENT_HELLO_PARSE_LIMIT_BYTES: usize = 64 * 1024;

fn write_shared_log(log: &SharedLog, message: &str) {
    if let Ok(mut file) = log.lock() {
        let _ = writeln!(file, "{message}");
    }
}

fn handle_connect_proxy_client(
    mut client: TcpStream,
    peer_addr: String,
    allowed_domains: Arc<Vec<String>>,
    log: SharedLog,
) {
    let request = match read_connect_proxy_request(&mut client) {
        Ok(request) => request,
        Err(err) => {
            let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
            write_shared_log(
                &log,
                &format!("proxy rejected malformed request from {peer_addr}: {err}"),
            );
            return;
        }
    };

    let (method, host, port) = match parse_connect_request(&request) {
        Ok(parts) => parts,
        Err(err) => {
            let status = if err.contains("CONNECT") {
                b"HTTP/1.1 405 Method Not Allowed\r\n\r\n".as_slice()
            } else {
                b"HTTP/1.1 400 Bad Request\r\n\r\n".as_slice()
            };
            let _ = client.write_all(status);
            write_shared_log(
                &log,
                &format!("proxy rejected bad request from {peer_addr}: {err}"),
            );
            return;
        }
    };

    if method != "CONNECT" {
        let _ = client.write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n");
        write_shared_log(
            &log,
            &format!("proxy rejected unsupported method from {peer_addr}: {method}"),
        );
        return;
    }

    if port != 443 {
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
        write_shared_log(
            &log,
            &format!("proxy denied {host}:{port} from {peer_addr}: only port 443 is allowed"),
        );
        return;
    }
    if !allowlisted_domain_matches(&allowed_domains, &host) {
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
        write_shared_log(
            &log,
            &format!("proxy denied {host}:{port} from {peer_addr}: hostname not allowlisted"),
        );
        return;
    }

    match TcpStream::connect((host.as_str(), port)) {
        Ok(mut upstream) => {
            if client
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .is_err()
            {
                write_shared_log(
                    &log,
                    &format!(
                        "proxy failed to acknowledge CONNECT for {host}:{port} from {peer_addr}"
                    ),
                );
                return;
            }
            write_shared_log(
                &log,
                &format!("proxy accepted {host}:{port} from {peer_addr}"),
            );
            if let Err(err) = tunnel_tcp_streams(client, &mut upstream) {
                write_shared_log(
                    &log,
                    &format!(
                        "proxy tunnel to {host}:{port} from {peer_addr} ended with error: {err}"
                    ),
                );
            } else {
                write_shared_log(
                    &log,
                    &format!("proxy tunnel to {host}:{port} from {peer_addr} closed cleanly"),
                );
            }
        }
        Err(err) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
            write_shared_log(
                &log,
                &format!("proxy upstream connect failed for {host}:{port} from {peer_addr}: {err}"),
            );
        }
    }
}

fn allowlisted_domain_matches(allowed_domains: &[String], host: &str) -> bool {
    match host_proxy_egress_policy(allowed_domains) {
        HostProxyEgressPolicy::AllowAllDebug => true,
        HostProxyEgressPolicy::Restricted(domains) => domains.iter().any(|allowed| allowed == host),
    }
}

fn read_connect_proxy_request(stream: &mut TcpStream) -> io::Result<String> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    while request.len() < PROXY_REQUEST_LIMIT_BYTES {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "connection closed before proxy request headers were complete; partial request={}",
                    format_proxy_request_preview(&request)
                ),
            ));
        }
        request.extend_from_slice(&chunk[..read]);
        if proxy_request_headers_complete(&request) {
            return Ok(String::from_utf8_lossy(&request).into_owned());
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "proxy request headers exceeded size limit; partial request={}",
            format_proxy_request_preview(&request)
        ),
    ))
}

fn proxy_request_headers_complete(request: &[u8]) -> bool {
    request.windows(4).any(|window| window == b"\r\n\r\n")
        || request.windows(2).any(|window| window == b"\n\n")
}

fn format_proxy_request_preview(request: &[u8]) -> String {
    const MAX_PREVIEW_BYTES: usize = 256;

    if request.is_empty() {
        return "<empty>".to_string();
    }

    let mut preview = String::new();
    for &byte in request.iter().take(MAX_PREVIEW_BYTES) {
        for escaped in std::ascii::escape_default(byte) {
            preview.push(escaped as char);
        }
    }
    if request.len() > MAX_PREVIEW_BYTES {
        preview.push_str("...");
    }
    preview
}

fn parse_connect_request(request: &str) -> Result<(String, String, u16), &'static str> {
    let first_line = request.lines().next().ok_or("empty request")?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().ok_or("missing HTTP method")?.to_string();
    let target = parts.next().ok_or("missing CONNECT target")?;
    let version = parts.next().ok_or("missing HTTP version")?;
    if !version.starts_with("HTTP/1.") {
        return Err("unsupported HTTP version");
    }
    let (host, port) = target
        .rsplit_once(':')
        .ok_or("CONNECT target must be host:port")?;
    let host = host
        .trim_matches(|ch| ch == '[' || ch == ']')
        .trim()
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err("CONNECT target host must not be empty");
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| "CONNECT target port must be numeric")?;
    Ok((method, host, port))
}

fn tunnel_tcp_streams(client: TcpStream, upstream: &mut TcpStream) -> io::Result<()> {
    let mut client_reader = client.try_clone()?;
    let mut client_writer = client;
    let mut upstream_writer = upstream.try_clone()?;
    let client_to_upstream = thread::spawn(move || {
        let result =
            copy_client_to_upstream_with_tls_version_gate(&mut client_reader, &mut upstream_writer);
        let _ = upstream_writer.shutdown(Shutdown::Write);
        result
    });

    let mut upstream_reader = upstream.try_clone()?;
    let upstream_to_client_result = io::copy(&mut upstream_reader, &mut client_writer);
    let _ = client_writer.shutdown(Shutdown::Write);
    let client_to_upstream_result = match client_to_upstream.join() {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::Other,
            "client-to-upstream proxy worker panicked",
        )),
    };

    if let Err(err) = client_to_upstream_result {
        return Err(err);
    }
    upstream_to_client_result.map(|_| ())
}

fn copy_client_to_upstream_with_tls_version_gate(
    client_reader: &mut TcpStream,
    upstream_writer: &mut TcpStream,
) -> io::Result<()> {
    let mut chunk = [0_u8; 4096];
    let mut buffered = Vec::new();
    let mut tls_version_validated = false;

    loop {
        let read = client_reader.read(&mut chunk)?;
        if read == 0 {
            if tls_version_validated {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "connection closed before TLS ClientHello was received",
            ));
        }

        if tls_version_validated {
            upstream_writer.write_all(&chunk[..read])?;
            continue;
        }

        buffered.extend_from_slice(&chunk[..read]);
        if buffered.len() > TLS_CLIENT_HELLO_PARSE_LIMIT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "TLS ClientHello exceeded {} bytes before validation",
                    TLS_CLIENT_HELLO_PARSE_LIMIT_BYTES
                ),
            ));
        }

        match parse_tls_client_hello_max_version(&buffered) {
            TlsClientHelloParseResult::NeedMoreData => continue,
            TlsClientHelloParseResult::Invalid(reason) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("failed to parse TLS ClientHello: {reason}"),
                ))
            }
            TlsClientHelloParseResult::Parsed(max_version) => {
                if max_version < TLS_MIN_ALLOWED_VERSION {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "offered TLS version {} is below minimum {}",
                            format_tls_version(max_version),
                            format_tls_version(TLS_MIN_ALLOWED_VERSION)
                        ),
                    ));
                }

                upstream_writer.write_all(&buffered)?;
                buffered.clear();
                tls_version_validated = true;
            }
        }
    }
}

enum TlsClientHelloParseResult {
    NeedMoreData,
    Parsed(u16),
    Invalid(&'static str),
}

fn parse_tls_client_hello_max_version(data: &[u8]) -> TlsClientHelloParseResult {
    if data.len() < 5 {
        return TlsClientHelloParseResult::NeedMoreData;
    }

    let mut record_offset = 0_usize;
    let mut handshake_payload = Vec::new();

    while record_offset < data.len() {
        let record_header_end = record_offset.saturating_add(5);
        if record_header_end > data.len() {
            return TlsClientHelloParseResult::NeedMoreData;
        }

        let content_type = data[record_offset];
        if content_type != TLS_HANDSHAKE_CONTENT_TYPE {
            return TlsClientHelloParseResult::Invalid(
                "first TLS record is not a handshake record",
            );
        }

        let record_len = read_u16(data, record_offset + 3) as usize;
        let record_payload_start = record_offset + 5;
        let record_payload_end = record_payload_start.saturating_add(record_len);
        if record_payload_end > data.len() {
            return TlsClientHelloParseResult::NeedMoreData;
        }

        handshake_payload.extend_from_slice(&data[record_payload_start..record_payload_end]);
        if handshake_payload.len() > TLS_CLIENT_HELLO_PARSE_LIMIT_BYTES {
            return TlsClientHelloParseResult::Invalid(
                "TLS handshake payload exceeded parser limit",
            );
        }

        if handshake_payload.len() >= 4 {
            if handshake_payload[0] != TLS_CLIENT_HELLO_HANDSHAKE_TYPE {
                return TlsClientHelloParseResult::Invalid(
                    "first TLS handshake message is not ClientHello",
                );
            }

            let client_hello_len = ((handshake_payload[1] as usize) << 16)
                | ((handshake_payload[2] as usize) << 8)
                | handshake_payload[3] as usize;
            if client_hello_len == 0 {
                return TlsClientHelloParseResult::Invalid("ClientHello payload must not be empty");
            }
            if client_hello_len > TLS_CLIENT_HELLO_PARSE_LIMIT_BYTES {
                return TlsClientHelloParseResult::Invalid(
                    "ClientHello payload exceeded parser limit",
                );
            }

            let client_hello_end = 4 + client_hello_len;
            if handshake_payload.len() < client_hello_end {
                record_offset = record_payload_end;
                continue;
            }

            let client_hello = &handshake_payload[4..client_hello_end];
            return match parse_client_hello_max_supported_version(client_hello) {
                Ok(version) => TlsClientHelloParseResult::Parsed(version),
                Err(reason) => TlsClientHelloParseResult::Invalid(reason),
            };
        }

        record_offset = record_payload_end;
    }

    TlsClientHelloParseResult::NeedMoreData
}

fn parse_client_hello_max_supported_version(client_hello: &[u8]) -> Result<u16, &'static str> {
    if client_hello.len() < 34 {
        return Err("ClientHello is truncated before legacy_version/random");
    }

    let legacy_version = read_u16(client_hello, 0);
    let mut cursor = 34;

    let session_id_len = *client_hello
        .get(cursor)
        .ok_or("ClientHello missing session_id length")? as usize;
    cursor += 1;
    if cursor + session_id_len > client_hello.len() {
        return Err("ClientHello session_id is truncated");
    }
    cursor += session_id_len;

    if cursor + 2 > client_hello.len() {
        return Err("ClientHello missing cipher_suites length");
    }
    let cipher_suites_len = read_u16(client_hello, cursor) as usize;
    if cipher_suites_len == 0 || cipher_suites_len % 2 != 0 {
        return Err("ClientHello cipher_suites length is invalid");
    }
    cursor += 2;
    if cursor + cipher_suites_len > client_hello.len() {
        return Err("ClientHello cipher_suites are truncated");
    }
    cursor += cipher_suites_len;

    let compression_methods_len = *client_hello
        .get(cursor)
        .ok_or("ClientHello missing compression_methods length")?
        as usize;
    if compression_methods_len == 0 {
        return Err("ClientHello compression_methods length is invalid");
    }
    cursor += 1;
    if cursor + compression_methods_len > client_hello.len() {
        return Err("ClientHello compression_methods are truncated");
    }
    cursor += compression_methods_len;

    if cursor == client_hello.len() {
        return Ok(legacy_version);
    }

    if cursor + 2 > client_hello.len() {
        return Err("ClientHello missing extensions length");
    }
    let extensions_len = read_u16(client_hello, cursor) as usize;
    cursor += 2;
    let extensions_end = cursor + extensions_len;
    if extensions_end > client_hello.len() {
        return Err("ClientHello extensions are truncated");
    }

    let mut max_supported_version: Option<u16> = None;
    while cursor < extensions_end {
        if cursor + 4 > extensions_end {
            return Err("ClientHello extension header is truncated");
        }

        let extension_type = read_u16(client_hello, cursor);
        let extension_len = read_u16(client_hello, cursor + 2) as usize;
        cursor += 4;
        if cursor + extension_len > extensions_end {
            return Err("ClientHello extension payload is truncated");
        }

        if extension_type == TLS_SUPPORTED_VERSIONS_EXTENSION {
            let supported_max =
                parse_supported_versions_extension(&client_hello[cursor..cursor + extension_len])?;
            max_supported_version = Some(match max_supported_version {
                Some(existing) => existing.max(supported_max),
                None => supported_max,
            });
        }
        cursor += extension_len;
    }

    if cursor != extensions_end {
        return Err("ClientHello extensions are malformed");
    }

    Ok(max_supported_version.unwrap_or(legacy_version))
}

fn parse_supported_versions_extension(extension: &[u8]) -> Result<u16, &'static str> {
    let list_len = *extension
        .first()
        .ok_or("supported_versions extension payload is empty")? as usize;
    if list_len == 0 || list_len % 2 != 0 {
        return Err("supported_versions extension length byte is invalid");
    }
    if extension.len() != 1 + list_len {
        return Err("supported_versions extension length does not match payload");
    }

    let mut max_supported_version: Option<u16> = None;
    for version in extension[1..].chunks_exact(2) {
        let parsed = u16::from_be_bytes([version[0], version[1]]);
        if is_grease_value(parsed) {
            continue;
        }
        max_supported_version = Some(match max_supported_version {
            Some(existing) => existing.max(parsed),
            None => parsed,
        });
    }

    max_supported_version.ok_or("supported_versions extension contained only GREASE values")
}

fn is_grease_value(value: u16) -> bool {
    let [hi, lo] = value.to_be_bytes();
    hi == lo && (hi & 0x0f) == 0x0a
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

fn format_tls_version(version: u16) -> String {
    let name = match version {
        0x0300 => "SSL 3.0",
        0x0301 => "TLS 1.0",
        0x0302 => "TLS 1.1",
        0x0303 => "TLS 1.2",
        0x0304 => "TLS 1.3",
        _ => "unknown",
    };
    format!("{name} (0x{version:04x})")
}

pub(super) fn allocate_network_lease(
    subnet_cidr: &str,
    handle_id: u64,
    tap_name_prefix: &str,
    proxy_port: u16,
) -> Result<NetworkLease, ExecutionError> {
    let (subnet_base, subnet_prefix) = parse_ipv4_cidr(subnet_cidr)?;
    let Some(handle_index) = handle_id.checked_sub(1) else {
        return Err(ExecutionError::RunnerState(format!(
            "invalid firecracker handle id {handle_id} for network lease allocation"
        )));
    };
    let available_subnets = 1_u64 << u32::from(30 - subnet_prefix);
    if handle_index >= available_subnets {
        return Err(ExecutionError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr {subnet_cidr} does not have enough /30 networks for handle id {handle_id}"
        )));
    }

    let block_base = subnet_base
        .checked_add(u32::try_from(handle_index * 4).map_err(|_| {
            ExecutionError::RunnerState(format!(
                "network handle id {handle_id} overflowed /30 block allocation"
            ))
        })?)
        .ok_or_else(|| {
            ExecutionError::RunnerState(format!(
                "network handle id {handle_id} overflowed IPv4 lease allocation"
            ))
        })?;
    let host_ip = u32_to_ipv4(block_base + 1);
    let guest_ip = u32_to_ipv4(block_base + 2);
    let mac_bytes = handle_id.to_be_bytes();
    let guest_mac = format!(
        "02:fc:{:02x}:{:02x}:{:02x}:{:02x}",
        mac_bytes[4], mac_bytes[5], mac_bytes[6], mac_bytes[7]
    );

    Ok(NetworkLease {
        tap_name: make_tap_name(tap_name_prefix, handle_id)?,
        guest_iface: "eth0".to_string(),
        host_ip,
        guest_ip,
        prefix_len: 30,
        guest_mac,
        proxy_port,
    })
}

pub(super) fn parse_ipv4_cidr(raw: &str) -> Result<(u32, u8), ExecutionError> {
    let (address, prefix) = raw.split_once('/').ok_or_else(|| {
        ExecutionError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must be an IPv4 CIDR like 172.22.0.0/16: {raw}"
        ))
    })?;
    let address = address.parse::<Ipv4Addr>().map_err(|_| {
        ExecutionError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must contain a valid IPv4 address: {raw}"
        ))
    })?;
    let prefix_len = prefix.parse::<u8>().map_err(|_| {
        ExecutionError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must contain a numeric prefix length: {raw}"
        ))
    })?;
    if prefix_len > 30 {
        return Err(ExecutionError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must have prefix length 30 or smaller: {raw}"
        )));
    }

    let address_u32 = ipv4_to_u32(address);
    let network_mask = if prefix_len == 0 {
        0
    } else {
        u32::MAX << u32::from(32 - prefix_len)
    };
    if address_u32 & !network_mask != 0 {
        return Err(ExecutionError::InvalidConfig(format!(
            "sandbox.firecracker.network.subnet_cidr must use a network base address aligned to its prefix: {raw}"
        )));
    }
    Ok((address_u32, prefix_len))
}

fn make_tap_name(prefix: &str, handle_id: u64) -> Result<String, ExecutionError> {
    let suffix = handle_id.to_string();
    if suffix.len() >= 15 {
        return Err(ExecutionError::InvalidConfig(format!(
            "firecracker handle id {handle_id} is too large to fit in a Linux tap interface name"
        )));
    }
    let max_prefix_len = 15 - suffix.len();
    let truncated_prefix = prefix.chars().take(max_prefix_len).collect::<String>();
    if truncated_prefix.is_empty() {
        return Err(ExecutionError::InvalidConfig(
            "sandbox.firecracker.network.tap_name_prefix must leave room for a numeric suffix"
                .to_string(),
        ));
    }
    Ok(format!("{truncated_prefix}{suffix}"))
}

fn ipv4_to_u32(address: Ipv4Addr) -> u32 {
    u32::from_be_bytes(address.octets())
}

fn u32_to_ipv4(address: u32) -> Ipv4Addr {
    Ipv4Addr::from(address.to_be_bytes())
}

pub(super) enum HostProxyEgressPolicy<'a> {
    Restricted(&'a [String]),
    AllowAllDebug,
}

pub(super) fn host_proxy_egress_policy(domains: &[String]) -> HostProxyEgressPolicy<'_> {
    if domains.iter().any(|domain| domain == "*") {
        HostProxyEgressPolicy::AllowAllDebug
    } else {
        HostProxyEgressPolicy::Restricted(domains)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        net::{TcpListener, TcpStream},
        thread,
    };

    #[test]
    fn parse_tls_client_hello_accepts_tls_1_2_without_supported_versions_extension() {
        let client_hello = build_tls_client_hello_record(0x0303, None);
        let parsed = parse_tls_client_hello_max_version(&client_hello);
        assert!(matches!(parsed, TlsClientHelloParseResult::Parsed(0x0303)));
    }

    #[test]
    fn parse_tls_client_hello_accepts_tls_1_3_with_supported_versions_extension() {
        let client_hello = build_tls_client_hello_record(0x0303, Some(&[0x0304]));
        let parsed = parse_tls_client_hello_max_version(&client_hello);
        assert!(matches!(parsed, TlsClientHelloParseResult::Parsed(0x0304)));
    }

    #[test]
    fn parse_tls_client_hello_ignores_grease_versions() {
        let client_hello = build_tls_client_hello_record(0x0301, Some(&[0x2a2a, 0x0302]));
        let parsed = parse_tls_client_hello_max_version(&client_hello);
        assert!(matches!(parsed, TlsClientHelloParseResult::Parsed(0x0302)));
    }

    #[test]
    fn parse_tls_client_hello_rejects_non_handshake_record() {
        let mut record = vec![23, 0x03, 0x03, 0x00, 0x01, 0x00];
        record.extend_from_slice(&[0x00; 8]);
        let parsed = parse_tls_client_hello_max_version(&record);
        assert!(matches!(
            parsed,
            TlsClientHelloParseResult::Invalid("first TLS record is not a handshake record")
        ));
    }

    #[test]
    fn parse_tls_client_hello_needs_more_data_for_partial_record() {
        let client_hello = build_tls_client_hello_record(0x0303, Some(&[0x0304]));
        let parsed = parse_tls_client_hello_max_version(&client_hello[..8]);
        assert!(matches!(parsed, TlsClientHelloParseResult::NeedMoreData));
    }

    #[test]
    fn tls_gate_blocks_client_hello_below_tls_1_2() {
        let (mut client_writer, mut proxy_reader) = connected_tcp_pair();
        let (mut proxy_writer, mut upstream_reader) = connected_tcp_pair();
        let worker = thread::spawn(move || {
            copy_client_to_upstream_with_tls_version_gate(&mut proxy_reader, &mut proxy_writer)
        });

        let client_hello_tls_1_1 = build_tls_client_hello_record(0x0302, None);
        client_writer
            .write_all(&client_hello_tls_1_1)
            .expect("write client hello");
        client_writer
            .shutdown(Shutdown::Write)
            .expect("shutdown writer");

        let err = worker
            .join()
            .expect("tls gate worker should not panic")
            .expect_err("TLS 1.1 should be blocked");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        let mut forwarded = Vec::new();
        upstream_reader
            .read_to_end(&mut forwarded)
            .expect("read forwarded bytes");
        assert!(forwarded.is_empty());
    }

    #[test]
    fn tls_gate_forwards_tls_1_2_and_follow_up_payload() {
        let (mut client_writer, mut proxy_reader) = connected_tcp_pair();
        let (mut proxy_writer, mut upstream_reader) = connected_tcp_pair();
        let worker = thread::spawn(move || {
            copy_client_to_upstream_with_tls_version_gate(&mut proxy_reader, &mut proxy_writer)
        });

        let mut payload = build_tls_client_hello_record(0x0303, None);
        payload.extend_from_slice(b"test-payload");
        client_writer.write_all(&payload).expect("write payload");
        client_writer
            .shutdown(Shutdown::Write)
            .expect("shutdown writer");

        worker
            .join()
            .expect("tls gate worker should not panic")
            .expect("TLS 1.2 should pass");

        let mut forwarded = Vec::new();
        upstream_reader
            .read_to_end(&mut forwarded)
            .expect("read forwarded bytes");
        assert_eq!(forwarded, payload);
    }

    fn build_tls_client_hello_record(
        legacy_version: u16,
        supported_versions: Option<&[u16]>,
    ) -> Vec<u8> {
        let mut client_hello = Vec::new();
        client_hello.extend_from_slice(&legacy_version.to_be_bytes());
        client_hello.extend_from_slice(&[0_u8; 32]);
        client_hello.push(0);
        client_hello.extend_from_slice(&2_u16.to_be_bytes());
        client_hello.extend_from_slice(&0x1301_u16.to_be_bytes());
        client_hello.push(1);
        client_hello.push(0);

        if let Some(supported_versions) = supported_versions {
            let mut extension_payload = Vec::new();
            extension_payload.push((supported_versions.len() * 2) as u8);
            for version in supported_versions {
                extension_payload.extend_from_slice(&version.to_be_bytes());
            }

            let mut extensions = Vec::new();
            extensions.extend_from_slice(&TLS_SUPPORTED_VERSIONS_EXTENSION.to_be_bytes());
            extensions.extend_from_slice(&(extension_payload.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&extension_payload);

            client_hello.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
            client_hello.extend_from_slice(&extensions);
        }

        let mut handshake = Vec::new();
        handshake.push(TLS_CLIENT_HELLO_HANDSHAKE_TYPE);
        let hello_len = client_hello.len();
        handshake.push(((hello_len >> 16) & 0xff) as u8);
        handshake.push(((hello_len >> 8) & 0xff) as u8);
        handshake.push((hello_len & 0xff) as u8);
        handshake.extend_from_slice(&client_hello);

        let mut record = Vec::new();
        record.push(TLS_HANDSHAKE_CONTENT_TYPE);
        record.extend_from_slice(&legacy_version.to_be_bytes());
        record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
        record.extend_from_slice(&handshake);
        record
    }

    fn connected_tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind local listener");
        let listener_addr = listener.local_addr().expect("listener local addr");
        let client = TcpStream::connect(listener_addr).expect("connect local listener");
        let (server, _) = listener.accept().expect("accept local connection");
        (client, server)
    }
}
