use std::fs;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to spawn {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("transport daemon did not become ready: {detail}")]
    Readiness { detail: String },
    #[error("failed to run command {program}: {detail}")]
    Command { program: String, detail: String },
}

#[derive(Debug)]
pub struct SshTransport {
    pub port: u16,
    pub user: String,
    pub client_key: PathBuf,
    pub known_hosts: PathBuf,
    pub ssh_bin: PathBuf,
    child: Child,
}

impl SshTransport {
    pub fn git_ssh_command(&self) -> String {
        format!(
            "{} -F /dev/null -i {} -o IdentitiesOnly=yes -o UserKnownHostsFile={} -o GlobalKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes -o BatchMode=yes -p {}",
            self.ssh_bin.display(),
            self.client_key.display(),
            self.known_hosts.display(),
            self.port,
        )
    }

    pub fn remote_url_for_repo(&self, repo_path: &Path) -> String {
        format!(
            "ssh://{}@127.0.0.1/{repo}",
            self.user,
            repo = repo_path.display()
        )
    }
}

impl Drop for SshTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
pub struct SmartHttpTransport {
    pub port: u16,
    pub username: String,
    pub password: String,
    child: Child,
}

impl SmartHttpTransport {
    pub fn remote_url_for_repo(&self, repo_name: &str) -> String {
        format!(
            "http://{}:{}@127.0.0.1:{}/{}",
            self.username, self.password, self.port, repo_name
        )
    }
}

impl Drop for SmartHttpTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
pub struct TransportHarness {
    pub ssh: SshTransport,
    pub smart_http: SmartHttpTransport,
}

impl TransportHarness {
    pub fn start(case_root: &Path, repo_root: &Path) -> Result<Self, TransportError> {
        let sshd_bin = find_binary("GIT_RELAY_PROOF_SSHD_BIN", "sshd")?;
        let ssh_bin = find_binary("GIT_RELAY_PROOF_SSH_BIN", "ssh")?;
        let ssh_keygen_bin = find_binary("GIT_RELAY_PROOF_SSH_KEYGEN_BIN", "ssh-keygen")?;
        let python_bin = find_binary("GIT_RELAY_PROOF_PYTHON_BIN", "python3")?;
        let git_http_backend_bin = find_git_http_backend()?;

        let ssh = start_sshd(
            case_root,
            &sshd_bin,
            &ssh_bin,
            &ssh_keygen_bin,
            current_user(),
        )?;
        probe_git_ssh_ready(repo_root, &ssh)?;

        let smart_http =
            start_smart_http(case_root, repo_root, &python_bin, &git_http_backend_bin)?;

        Ok(Self { ssh, smart_http })
    }
}

fn start_sshd(
    case_root: &Path,
    sshd_bin: &Path,
    ssh_bin: &Path,
    ssh_keygen_bin: &Path,
    user: String,
) -> Result<SshTransport, TransportError> {
    let ssh_root = case_root.join("transport-ssh");
    fs::create_dir_all(&ssh_root).map_err(|source| TransportError::CreateDir {
        path: ssh_root.clone(),
        source,
    })?;

    let host_key = ssh_root.join("host_ed25519");
    let client_key = ssh_root.join("client_ed25519");
    let authorized_keys = ssh_root.join("authorized_keys");
    let config_path = ssh_root.join("sshd_config");
    let log_path = ssh_root.join("sshd.log");
    let known_hosts = ssh_root.join("known_hosts");
    let pid_path = ssh_root.join("sshd.pid");

    run_command(
        ssh_keygen_bin,
        &[
            "-t".to_owned(),
            "ed25519".to_owned(),
            "-N".to_owned(),
            "".to_owned(),
            "-f".to_owned(),
            host_key.display().to_string(),
        ],
    )?;
    run_command(
        ssh_keygen_bin,
        &[
            "-t".to_owned(),
            "ed25519".to_owned(),
            "-N".to_owned(),
            "".to_owned(),
            "-f".to_owned(),
            client_key.display().to_string(),
        ],
    )?;

    let client_pub_path = PathBuf::from(format!("{}.pub", client_key.display()));
    let client_pub =
        fs::read_to_string(&client_pub_path).map_err(|source| TransportError::Read {
            path: client_pub_path.clone(),
            source,
        })?;
    fs::write(&authorized_keys, client_pub).map_err(|source| TransportError::Write {
        path: authorized_keys.clone(),
        source,
    })?;

    let port = pick_free_port()?;
    let config = format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {host_key}\nPidFile {pid_file}\nAuthorizedKeysFile {authorized_keys}\nPubkeyAuthentication yes\nPasswordAuthentication no\nChallengeResponseAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nPermitUserEnvironment no\nAllowTcpForwarding no\nX11Forwarding no\nPermitTTY no\nPrintMotd no\nLogLevel ERROR\n",
        host_key = host_key.display(),
        pid_file = pid_path.display(),
        authorized_keys = authorized_keys.display(),
    );
    fs::write(&config_path, config).map_err(|source| TransportError::Write {
        path: config_path.clone(),
        source,
    })?;

    let mut child = Command::new(sshd_bin)
        .arg("-D")
        .arg("-f")
        .arg(&config_path)
        .arg("-E")
        .arg(&log_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| TransportError::Spawn {
            program: sshd_bin.display().to_string(),
            source,
        })?;

    wait_for_port(port)?;

    let host_pub = fs::read_to_string(format!("{}.pub", host_key.display())).map_err(|source| {
        TransportError::Read {
            path: PathBuf::from(format!("{}.pub", host_key.display())),
            source,
        }
    })?;
    let known_hosts_entry = format!("[127.0.0.1]:{port} {}", host_pub.trim());
    fs::write(&known_hosts, format!("{known_hosts_entry}\n")).map_err(|source| {
        TransportError::Write {
            path: known_hosts.clone(),
            source,
        }
    })?;

    if let Some(status) = child.try_wait().map_err(|source| TransportError::Spawn {
        program: sshd_bin.display().to_string(),
        source,
    })? {
        return Err(TransportError::Readiness {
            detail: format!("sshd exited early with status {status}"),
        });
    }

    Ok(SshTransport {
        port,
        user,
        client_key,
        known_hosts,
        ssh_bin: ssh_bin.to_path_buf(),
        child,
    })
}

fn start_smart_http(
    case_root: &Path,
    repo_root: &Path,
    python_bin: &Path,
    git_http_backend_bin: &Path,
) -> Result<SmartHttpTransport, TransportError> {
    let http_root = case_root.join("transport-http");
    fs::create_dir_all(&http_root).map_err(|source| TransportError::CreateDir {
        path: http_root.clone(),
        source,
    })?;

    let script_path = http_root.join("smart_http_server.py");
    fs::write(&script_path, SMART_HTTP_BRIDGE).map_err(|source| TransportError::Write {
        path: script_path.clone(),
        source,
    })?;

    let port = pick_free_port()?;
    let username = format!("proof-user-{}", std::process::id());
    let password = format!("proof-pass-{}-{}", std::process::id(), current_time_ms());

    let mut child = Command::new(python_bin)
        .arg(&script_path)
        .arg(repo_root)
        .arg(port.to_string())
        .arg(&username)
        .arg(&password)
        .arg(git_http_backend_bin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| TransportError::Spawn {
            program: python_bin.display().to_string(),
            source,
        })?;

    wait_for_port(port)?;

    if let Some(status) = child.try_wait().map_err(|source| TransportError::Spawn {
        program: python_bin.display().to_string(),
        source,
    })? {
        return Err(TransportError::Readiness {
            detail: format!("smart-http bridge exited early with status {status}"),
        });
    }

    Ok(SmartHttpTransport {
        port,
        username,
        password,
        child,
    })
}

fn find_binary(env_var: &str, fallback: &str) -> Result<PathBuf, TransportError> {
    if let Ok(value) = std::env::var(env_var) {
        return Ok(PathBuf::from(value));
    }

    let output = Command::new("which")
        .arg(fallback)
        .output()
        .map_err(|source| TransportError::Spawn {
            program: "which".to_owned(),
            source,
        })?;

    if output.status.success() {
        let resolved = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !resolved.is_empty() {
            return Ok(PathBuf::from(resolved));
        }
    }

    Ok(PathBuf::from(fallback))
}

fn find_git_http_backend() -> Result<PathBuf, TransportError> {
    if let Ok(value) = std::env::var("GIT_RELAY_PROOF_GIT_HTTP_BACKEND_BIN") {
        return Ok(PathBuf::from(value));
    }

    let output = Command::new("git")
        .arg("--exec-path")
        .output()
        .map_err(|source| TransportError::Spawn {
            program: "git".to_owned(),
            source,
        })?;
    if !output.status.success() {
        return Err(TransportError::Command {
            program: "git --exec-path".to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    let exec_path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(Path::new(&exec_path).join("git-http-backend"))
}

fn run_command(program: &Path, args: &[String]) -> Result<(), TransportError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| TransportError::Spawn {
            program: program.display().to_string(),
            source,
        })?;

    if output.status.success() {
        return Ok(());
    }

    let detail = [
        String::from_utf8_lossy(&output.stderr).to_string(),
        String::from_utf8_lossy(&output.stdout).to_string(),
    ]
    .join(" | ");
    Err(TransportError::Command {
        program: format!("{} {:?}", program.display(), args),
        detail,
    })
}

fn probe_git_ssh_ready(repo_root: &Path, ssh: &SshTransport) -> Result<(), TransportError> {
    let probe_repo = repo_root.join("relay-authoritative.git");
    let output = Command::new("git")
        .env("GIT_SSH_COMMAND", ssh.git_ssh_command())
        .arg("ls-remote")
        .arg(ssh.remote_url_for_repo(&probe_repo))
        .arg("HEAD")
        .output()
        .map_err(|source| TransportError::Spawn {
            program: "git".to_owned(),
            source,
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if stderr.is_empty() && stdout.is_empty() {
        "git ls-remote over SSH failed without output".to_owned()
    } else if stderr.is_empty() {
        stdout
    } else if stdout.is_empty() {
        stderr
    } else {
        format!("{stderr} | {stdout}")
    };

    Err(TransportError::Readiness {
        detail: format!(
            "git-over-ssh readiness probe against {} failed: {detail}",
            probe_repo.display()
        ),
    })
}

fn pick_free_port() -> Result<u16, TransportError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|source| TransportError::Spawn {
        program: "bind".to_owned(),
        source,
    })?;
    let address = listener
        .local_addr()
        .map_err(|source| TransportError::Spawn {
            program: "local_addr".to_owned(),
            source,
        })?;
    Ok(address.port())
}

fn wait_for_port(port: u16) -> Result<(), TransportError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(TransportError::Readiness {
        detail: format!("port {port} did not become reachable before timeout"),
    })
}

fn current_user() -> String {
    let output = Command::new("id").arg("-un").output();
    if let Ok(capture) = output {
        if capture.status.success() {
            let value = String::from_utf8_lossy(&capture.stdout).trim().to_owned();
            if !value.is_empty() {
                return value;
            }
        }
    }

    if let Ok(user) = std::env::var("USER") {
        if !user.trim().is_empty() {
            return user;
        }
    }
    if let Ok(user) = std::env::var("LOGNAME") {
        if !user.trim().is_empty() {
            return user;
        }
    }

    "unknown".to_owned()
}

fn current_time_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

const SMART_HTTP_BRIDGE: &str = r#"#!/usr/bin/env python3
import base64
import os
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

repo_root = sys.argv[1]
port = int(sys.argv[2])
username = sys.argv[3]
password = sys.argv[4]
backend = sys.argv[5]

expected = "Basic " + base64.b64encode(f"{username}:{password}".encode()).decode()

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.handle_all()

    def do_POST(self):
        self.handle_all()

    def log_message(self, fmt, *args):
        pass

    def handle_all(self):
        auth = self.headers.get("Authorization", "")
        if auth != expected:
            self.send_response(401)
            self.send_header("WWW-Authenticate", "Basic realm=\"git-relay-proof\"")
            self.end_headers()
            return

        length = int(self.headers.get("Content-Length", "0") or "0")
        body = self.rfile.read(length) if length else b""
        path, _, query = self.path.partition("?")

        env = os.environ.copy()
        env.update({
            "GIT_PROJECT_ROOT": repo_root,
            "GIT_HTTP_EXPORT_ALL": "1",
            "REQUEST_METHOD": self.command,
            "PATH_INFO": path,
            "QUERY_STRING": query,
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "CONTENT_LENGTH": str(length),
            "REMOTE_USER": username,
            "AUTH_TYPE": "Basic",
            "REMOTE_ADDR": "127.0.0.1",
            "SERVER_PROTOCOL": self.request_version,
        })

        proc = subprocess.run([backend], input=body, env=env, capture_output=True)
        raw = proc.stdout
        header_end = raw.find(b"\r\n\r\n")
        sep_len = 4
        if header_end == -1:
            header_end = raw.find(b"\n\n")
            sep_len = 2
        if header_end == -1:
            self.send_response(500)
            self.end_headers()
            self.wfile.write(proc.stderr)
            return

        headers_blob = raw[:header_end].decode("utf-8", errors="replace")
        body = raw[header_end + sep_len:]

        status = 200
        headers = []
        for line in headers_blob.splitlines():
            if not line.strip():
                continue
            if line.lower().startswith("status:"):
                tokens = line.split(":", 1)[1].strip().split()
                if tokens:
                    try:
                        status = int(tokens[0])
                    except ValueError:
                        status = 200
            else:
                key, _, value = line.partition(":")
                headers.append((key.strip(), value.strip()))

        self.send_response(status)
        for key, value in headers:
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(body)

HTTPServer(("127.0.0.1", port), Handler).serve_forever()
"#;
