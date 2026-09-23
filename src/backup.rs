use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{ChaCha20Poly1305, Nonce, aead::Aead, aead::KeyInit};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::{connection::ConnectionProfile, secrets};

const MAGIC: &[u8; 8] = b"SQLMENC\0";
const VERSION: u8 = 1;
const HEADER_LENGTH: usize = MAGIC.len() + 1;
const SALT_LENGTH: usize = 16;
const NONCE_LENGTH: usize = 12;
const KEY_LENGTH: usize = 32;
const ARGON_MEMORY_KIB: u32 = 19 * 1024;
const ARGON_ITERATIONS: u32 = 2;
const ARGON_LANES: u32 = 1;
const MAX_BACKUP_BYTES: usize = 16 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
struct BackupPayload {
    version: u8,
    profiles: Vec<BackupProfile>,
}

#[derive(Deserialize, Serialize)]
struct BackupProfile {
    profile: ConnectionProfile,
    password: Option<String>,
}

impl Drop for BackupProfile {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

pub fn encrypt_profiles(profiles: &[ConnectionProfile], password: &str) -> Result<Vec<u8>, String> {
    let profiles = profiles
        .iter()
        .map(|profile| {
            let password = secrets::load_password(profile.id)
                .map_err(|error| format!("Could not read a connection password: {error}"))?;
            Ok(BackupProfile {
                profile: profile.clone(),
                password,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    encrypt_payload(
        &BackupPayload {
            version: VERSION,
            profiles,
        },
        password,
    )
}

fn encrypt_payload(payload: &BackupPayload, password: &str) -> Result<Vec<u8>, String> {
    let plaintext = Zeroizing::new(serde_json::to_vec(payload).map_err(|error| error.to_string())?);

    let mut salt = [0_u8; SALT_LENGTH];
    let mut nonce = [0_u8; NONCE_LENGTH];
    getrandom::fill(&mut salt).map_err(|error| error.to_string())?;
    getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;

    let key = derive_key(password, &salt)?;

    let mut header = Vec::with_capacity(HEADER_LENGTH);
    header.extend_from_slice(MAGIC);
    header.push(VERSION);

    let cipher = ChaCha20Poly1305::new_from_slice(&*key).map_err(|error| error.to_string())?;
    let nonce = Nonce::try_from(&nonce[..]).expect("nonce array has the required length");
    let ciphertext = cipher
        .encrypt(
            &nonce,
            chacha20poly1305::aead::Payload {
                msg: &plaintext,
                aad: &header,
            },
        )
        .map_err(|_| String::from("Could not encrypt the connection backup"))?;

    let mut output = header;
    output.extend_from_slice(&salt);
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

fn decrypt_payload(data: &[u8], password: &str) -> Result<BackupPayload, String> {
    let minimum_length = HEADER_LENGTH + SALT_LENGTH + NONCE_LENGTH + 16;
    if data.len() < minimum_length {
        return Err(String::from("This file is not a valid SQL Manager backup"));
    }
    if data.len() > MAX_BACKUP_BYTES {
        return Err(String::from("This backup file is too large to import"));
    }
    if &data[..MAGIC.len()] != MAGIC {
        return Err(String::from("This file is not a SQL Manager backup"));
    }
    if data[MAGIC.len()] != VERSION {
        return Err(String::from("This backup format version is not supported"));
    }

    let header = &data[..HEADER_LENGTH];
    let salt_start = HEADER_LENGTH;
    let nonce_start = salt_start + SALT_LENGTH;
    let ciphertext_start = nonce_start + NONCE_LENGTH;
    let salt = &data[salt_start..nonce_start];
    let nonce = &data[nonce_start..ciphertext_start];
    let ciphertext = &data[ciphertext_start..];

    let key = derive_key(password, salt)?;

    let cipher = ChaCha20Poly1305::new_from_slice(&*key).map_err(|error| error.to_string())?;
    let nonce = Nonce::try_from(nonce).map_err(|_| String::from("Backup nonce is invalid"))?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                &nonce,
                chacha20poly1305::aead::Payload {
                    msg: ciphertext,
                    aad: header,
                },
            )
            .map_err(|_| String::from("Wrong password or damaged backup file"))?,
    );

    let payload: BackupPayload = serde_json::from_slice(&plaintext)
        .map_err(|_| String::from("Backup contents are invalid"))?;
    if payload.version != VERSION {
        return Err(String::from(
            "Backup contents use an unsupported format version",
        ));
    }

    Ok(payload)
}

fn derive_key(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LENGTH]>, String> {
    let params = Params::new(
        ARGON_MEMORY_KIB,
        ARGON_ITERATIONS,
        ARGON_LANES,
        Some(KEY_LENGTH),
    )
    .map_err(|error| error.to_string())?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0_u8; KEY_LENGTH]);
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut *key)
        .map_err(|error| error.to_string())?;
    Ok(key)
}

pub fn decrypt_profiles(
    data: &[u8],
    password: &str,
) -> Result<Vec<(ConnectionProfile, Option<String>)>, String> {
    let payload = decrypt_payload(data, password)?;

    Ok(payload
        .profiles
        .into_iter()
        .map(|mut item| (item.profile.clone(), item.password.take()))
        .collect())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::{
        connection::{ConnectionProfile, SshTunnelConfig, TlsMode},
        engine::EngineKind,
    };

    use super::{
        BackupPayload, BackupProfile, VERSION, decrypt_payload, decrypt_profiles, encrypt_payload,
        encrypt_profiles,
    };

    fn test_profile() -> ConnectionProfile {
        ConnectionProfile {
            id: Uuid::new_v4(),
            engine: EngineKind::PostgreSql,
            name: String::from("Local"),
            host: String::from("localhost"),
            port: 5432,
            database: String::from("postgres"),
            username: String::from("postgres"),
            tls_mode: TlsMode::Require,
            ssh_tunnel: Some(SshTunnelConfig {
                host: String::from("bastion.example.com"),
                port: 22,
                username: String::from("dbuser"),
                identity_file: String::from("~/.ssh/id_ed25519"),
            }),
        }
    }

    #[test]
    fn rejects_files_with_invalid_magic() {
        assert!(decrypt_profiles(b"not a backup file", "secret").is_err());
    }

    #[test]
    fn rejects_tampered_ciphertext() {
        let mut data = encrypt_profiles(&[], "secret").expect("encrypt empty backup");
        let last = data.last_mut().expect("backup has ciphertext");
        *last ^= 1;

        assert!(decrypt_profiles(&data, "secret").is_err());
    }

    #[test]
    fn rejects_a_wrong_password() {
        let data =
            encrypt_profiles(&[], "correct horse battery staple").expect("encrypt empty backup");

        assert!(decrypt_profiles(&data, "wrong password").is_err());
    }

    #[test]
    fn encrypts_connection_metadata_without_storing_plaintext() {
        let profile = test_profile();
        let encrypted = encrypt_payload(
            &BackupPayload {
                version: VERSION,
                profiles: vec![BackupProfile {
                    profile: profile.clone(),
                    password: Some(String::from("postgres-password")),
                }],
            },
            "backup passphrase",
        )
        .expect("encrypt backup");
        let restored = decrypt_payload(&encrypted, "backup passphrase").expect("decrypt backup");

        assert_eq!(restored.profiles.len(), 1);
        assert_eq!(restored.profiles[0].profile.id, profile.id);
        assert_eq!(restored.profiles[0].profile.host, profile.host);
        assert_eq!(
            restored.profiles[0]
                .profile
                .ssh_tunnel
                .as_ref()
                .expect("restored SSH settings")
                .host,
            "bastion.example.com"
        );
        assert_eq!(
            restored.profiles[0].password.as_deref(),
            Some("postgres-password")
        );
        assert!(!String::from_utf8_lossy(&encrypted).contains("localhost"));
        assert!(!String::from_utf8_lossy(&encrypted).contains("postgres-password"));
    }
}
