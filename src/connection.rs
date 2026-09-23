use std::future::Future;

use serde::{Deserialize, Serialize};
use tokio_postgres::{Client, Config, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConnectionProfile {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub tls_mode: TlsMode,
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

        Ok(ConnectionProfile {
            id: existing_id.unwrap_or_else(Uuid::new_v4),
            name: name.to_owned(),
            host: host.to_owned(),
            port,
            database: database.to_owned(),
            username: username.to_owned(),
            tls_mode: self.tls_mode,
        })
    }
}

pub async fn test_connection(
    profile: &ConnectionProfile,
    password: &str,
) -> Result<String, tokio_postgres::Error> {
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
        let (client, connection) = config.connect(tls).await?;
        verify_server(client, connection).await
    } else {
        let (client, connection) = config.connect(tokio_postgres::NoTls).await?;
        verify_server(client, connection).await
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
    fn tls_defaults_to_verified_required_mode() {
        assert_eq!(TlsMode::default(), TlsMode::Require);
        assert_eq!(TlsMode::Require.label(), "Require and verify");
    }
}
