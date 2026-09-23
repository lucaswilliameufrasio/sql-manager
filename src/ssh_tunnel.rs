use std::{
    net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::connection::SshTunnelConfig;

pub struct SshTunnel {
    child: Child,
    pub local_port: u16,
}

impl SshTunnel {
    pub fn start(
        config: &SshTunnelConfig,
        database_host: &str,
        database_port: u16,
    ) -> Result<Self, String> {
        validate_ssh_value(&config.host, "SSH host")?;
        validate_ssh_value(&config.username, "SSH username")?;
        if config.port == 0 || database_port == 0 {
            return Err(String::from(
                "SSH and PostgreSQL ports must be greater than 0",
            ));
        }

        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())?;
        let local_port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        drop(listener);

        let remote_host = format_remote_host(database_host);
        let forward = format!("127.0.0.1:{local_port}:{remote_host}:{database_port}");
        let destination = format!("{}@{}", config.username, format_remote_host(&config.host));
        let mut command = Command::new("ssh");
        command
            .arg("-N")
            .arg("-p")
            .arg(config.port.to_string())
            .arg("-L")
            .arg(forward)
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-o")
            .arg("ServerAliveInterval=30")
            .arg("-o")
            .arg("ConnectTimeout=10");
        if !config.identity_file.trim().is_empty() {
            command
                .arg("-i")
                .arg(config.identity_file.trim())
                .arg("-o")
                .arg("IdentitiesOnly=yes");
        }
        let mut child = command
            .arg(destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Could not start OpenSSH: {error}"))?;

        let address = SocketAddr::from(([127, 0, 0, 1], local_port));
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!(
                    "OpenSSH tunnel exited before becoming ready ({status})"
                ));
            }

            if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
                return Ok(Self { child, local_port });
            }

            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(String::from(
                    "Timed out waiting for the SSH tunnel to become ready",
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn validate_ssh_value(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{label} is required"))
    } else if value.chars().any(char::is_whitespace) {
        Err(format!("{label} cannot contain whitespace"))
    } else {
        Ok(())
    }
}

fn format_remote_host(host: &str) -> String {
    match host.parse::<Ipv6Addr>() {
        Ok(address) => format!("[{address}]"),
        Err(_) => host.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{format_remote_host, validate_ssh_value};

    #[test]
    fn brackets_ipv6_remote_hosts_for_openssh_forwarding() {
        assert_eq!(format_remote_host("2001:db8::1"), "[2001:db8::1]");
        assert_eq!(format_remote_host("db.example.com"), "db.example.com");
    }

    #[test]
    fn rejects_empty_or_whitespace_ssh_values() {
        assert!(validate_ssh_value("", "SSH host").is_err());
        assert!(validate_ssh_value("host name", "SSH host").is_err());
        assert!(validate_ssh_value("db.example.com", "SSH host").is_ok());
    }
}
