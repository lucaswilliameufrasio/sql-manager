use std::future::Future;

use serde::{Deserialize, Serialize};
use tokio_postgres::{Client, Config, config::SslMode, tls::MakeTlsConnect};
use tokio_postgres_rustls::MakeRustlsConnect;
use uuid::Uuid;

use crate::ssh_tunnel::SshTunnel;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SshTunnelConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub identity_file: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConnectionProfile {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub tls_mode: TlsMode,
    #[serde(default)]
    pub ssh_tunnel: Option<SshTunnelConfig>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    #[default]
    Require,
    Prefer,
    Disable,
}

impl TlsMode {
    pub const ALL: [Self; 3] = [Self::Require, Self::Prefer, Self::Disable];

    pub fn label(self) -> &'static str {
        match self {
            Self::Require => "Require and verify",
            Self::Prefer => "Prefer SSL/TLS",
            Self::Disable => "Disable SSL/TLS",
        }
    }

    fn postgres_mode(self) -> SslMode {
        match self {
            Self::Require => SslMode::Require,
            Self::Prefer => SslMode::Prefer,
            Self::Disable => SslMode::Disable,
        }
    }
}

pub struct ConnectionDraft {
    pub name: String,
    pub host: String,
    pub port: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub tls_mode: TlsMode,
    pub ssh_enabled: bool,
    pub ssh_host: String,
    pub ssh_port: String,
    pub ssh_username: String,
    pub ssh_identity_file: String,
}

impl Default for ConnectionDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: String::from("5432"),
            database: String::new(),
            username: String::new(),
            password: String::new(),
            tls_mode: TlsMode::default(),
            ssh_enabled: false,
            ssh_host: String::new(),
            ssh_port: String::from("22"),
            ssh_username: String::new(),
            ssh_identity_file: String::new(),
        }
    }
}

impl ConnectionDraft {
    pub fn from(profile: &ConnectionProfile) -> Self {
        Self {
            name: profile.name.clone(),
            host: profile.host.clone(),
            port: profile.port.to_string(),
            database: profile.database.clone(),
            username: profile.username.clone(),
            password: String::new(),
            tls_mode: profile.tls_mode,
            ssh_enabled: profile.ssh_tunnel.is_some(),
            ssh_host: profile
                .ssh_tunnel
                .as_ref()
                .map_or_else(String::new, |ssh| ssh.host.clone()),
            ssh_port: profile
                .ssh_tunnel
                .as_ref()
                .map_or_else(|| String::from("22"), |ssh| ssh.port.to_string()),
            ssh_username: profile
                .ssh_tunnel
                .as_ref()
                .map_or_else(String::new, |ssh| ssh.username.clone()),
            ssh_identity_file: profile
                .ssh_tunnel
                .as_ref()
                .map_or_else(String::new, |ssh| ssh.identity_file.clone()),
        }
    }

    pub fn to_profile(&self, existing_id: Option<Uuid>) -> Result<ConnectionProfile, String> {
        let name = self.name.trim();
        let host = self.host.trim();
        let database = self.database.trim();
        let username = self.username.trim();
        let port = self
            .port
            .trim()
            .parse::<u16>()
            .map_err(|_| String::from("Port must be a number between 1 and 65535"))?;

        if name.is_empty() || host.is_empty() || database.is_empty() || username.is_empty() {
            return Err(String::from(
                "Name, host, database, and username are required",
            ));
        }
        if port == 0 {
            return Err(String::from("Port must be greater than 0"));
        }

        let ssh_tunnel = if self.ssh_enabled {
            let ssh_host = self.ssh_host.trim();
            let ssh_username = self.ssh_username.trim();
            let ssh_port = self
                .ssh_port
                .trim()
                .parse::<u16>()
                .map_err(|_| String::from("SSH port must be a number between 1 and 65535"))?;
            if ssh_port == 0 {
                return Err(String::from("SSH port must be greater than 0"));
            }
            if ssh_host.is_empty() || ssh_username.is_empty() {
                return Err(String::from("SSH host and username are required"));
            }
            Some(SshTunnelConfig {
                host: ssh_host.to_owned(),
                port: ssh_port,
                username: ssh_username.to_owned(),
                identity_file: self.ssh_identity_file.trim().to_owned(),
            })
        } else {
            None
        };

        Ok(ConnectionProfile {
            id: existing_id.unwrap_or_else(Uuid::new_v4),
            name: name.to_owned(),
            host: host.to_owned(),
            port,
            database: database.to_owned(),
            username: username.to_owned(),
            tls_mode: self.tls_mode,
            ssh_tunnel,
        })
    }
}

pub async fn test_connection(
    profile: &ConnectionProfile,
    password: &str,
) -> Result<String, String> {
    let tunnel = profile
        .ssh_tunnel
        .as_ref()
        .map(|ssh| SshTunnel::start(ssh, &profile.host, profile.port))
        .transpose()?;
    let mut config = Config::new();
    config
        .host(&profile.host)
        .port(profile.port)
        .dbname(&profile.database)
        .user(&profile.username)
        .password(password)
        .ssl_mode(profile.tls_mode.postgres_mode());

    if profile.tls_mode != TlsMode::Disable {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tls = MakeRustlsConnect::with_webpki_roots();
        if let Some(tunnel) = &tunnel {
            let stream = tokio::net::TcpStream::connect(("127.0.0.1", tunnel.local_port))
                .await
                .map_err(|error| error.to_string())?;
            let mut tls = MakeRustlsConnect::with_webpki_roots();
            let connector =
                <MakeRustlsConnect as MakeTlsConnect<tokio::net::TcpStream>>::make_tls_connect(
                    &mut tls,
                    &profile.host,
                )
                .expect("Rustls TLS connector is infallible");
            let (client, connection) = config
                .connect_raw(stream, connector)
                .await
                .map_err(|error| error.to_string())?;
            verify_server(client, connection)
                .await
                .map_err(|error| error.to_string())
        } else {
            let (client, connection) = config
                .connect(tls)
                .await
                .map_err(|error| error.to_string())?;
            verify_server(client, connection)
                .await
                .map_err(|error| error.to_string())
        }
    } else {
        if let Some(tunnel) = &tunnel {
            let stream = tokio::net::TcpStream::connect(("127.0.0.1", tunnel.local_port))
                .await
                .map_err(|error| error.to_string())?;
            let (client, connection) = config
                .connect_raw(stream, tokio_postgres::NoTls)
                .await
                .map_err(|error| error.to_string())?;
            verify_server(client, connection)
                .await
                .map_err(|error| error.to_string())
        } else {
            let (client, connection) = config
                .connect(tokio_postgres::NoTls)
                .await
                .map_err(|error| error.to_string())?;
            verify_server(client, connection)
                .await
                .map_err(|error| error.to_string())
        }
    }
}

async fn verify_server<C>(client: Client, connection: C) -> Result<String, tokio_postgres::Error>
where
    C: Future<Output = Result<(), tokio_postgres::Error>> + Send + 'static,
{
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let row = client.query_one("SELECT version()", &[]).await?;
    Ok(row.get(0))
}

#[cfg(test)]
mod tests {
    use super::{ConnectionDraft, TlsMode};

    #[test]
    fn draft_requires_connection_fields_and_valid_port() {
        let draft = ConnectionDraft::default();
        assert!(draft.to_profile(None).is_err());
    }

    #[test]
    fn valid_draft_creates_profile_with_secure_defaults() {
        let draft = ConnectionDraft {
            name: String::from("Local"),
            host: String::from("localhost"),
            database: String::from("postgres"),
            username: String::from("postgres"),
            ..ConnectionDraft::default()
        };

        let profile = draft.to_profile(None).expect("valid profile");

        assert_eq!(profile.port, 5432);
        assert_eq!(profile.tls_mode, TlsMode::Require);
        assert!(
            !serde_json::to_string(&profile)
                .expect("serialize profile")
                .contains("password")
        );
    }

    #[test]
    fn draft_rejects_invalid_port() {
        let draft = ConnectionDraft {
            name: String::from("Local"),
            host: String::from("localhost"),
            port: String::from("70000"),
            database: String::from("postgres"),
            username: String::from("postgres"),
            ..ConnectionDraft::default()
        };

        assert!(draft.to_profile(None).is_err());
    }

    #[test]
    fn ssh_tunnel_is_optional_and_validated_when_enabled() {
        let mut draft = ConnectionDraft {
            name: String::from("Remote"),
            host: String::from("db.internal"),
            database: String::from("app"),
            username: String::from("app_user"),
            ..ConnectionDraft::default()
        };
        assert!(
            draft
                .to_profile(None)
                .expect("direct connection")
                .ssh_tunnel
                .is_none()
        );

        draft.ssh_enabled = true;
        assert!(draft.to_profile(None).is_err());
        draft.ssh_host = String::from("bastion.example.com");
        draft.ssh_username = String::from("lucas");
        draft.ssh_identity_file = String::from("/home/user/.ssh/id_ed25519");
        let profile = draft.to_profile(None).expect("valid SSH connection");
        assert_eq!(profile.ssh_tunnel.expect("SSH config").port, 22);
    }

    #[test]
    fn tls_defaults_to_verified_required_mode() {
        assert_eq!(TlsMode::default(), TlsMode::Require);
        assert_eq!(TlsMode::Require.label(), "Require and verify");
    }
}
