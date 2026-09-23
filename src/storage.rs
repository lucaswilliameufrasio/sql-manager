use std::{fs, io, path::PathBuf};

use directories::ProjectDirs;
use serde_json::Error as JsonError;

use crate::connection::ConnectionProfile;

fn profiles_path() -> Result<PathBuf, String> {
    ProjectDirs::from("com", "Eufrasio", "SQL Manager")
        .map(|directories| directories.config_dir().join("connections.json"))
        .ok_or_else(|| String::from("Could not locate the application config directory"))
}

pub fn load_profiles() -> Result<Vec<ConnectionProfile>, String> {
    let path = profiles_path()?;
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error: JsonError| error.to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.to_string()),
    }
}

pub fn save_profile(
    profile: &ConnectionProfile,
    profiles: &mut Vec<ConnectionProfile>,
) -> Result<(), String> {
    let mut updated_profiles = profiles.clone();
    if let Some(existing) = updated_profiles
        .iter_mut()
        .find(|item| item.id == profile.id)
    {
        *existing = profile.clone();
    } else {
        updated_profiles.push(profile.clone());
    }

    save_profiles(&updated_profiles)?;
    *profiles = updated_profiles;
    Ok(())
}

pub fn save_profiles(profiles: &[ConnectionProfile]) -> Result<(), String> {
    let path = profiles_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| String::from("Could not determine the config directory"))?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;

    let bytes = serde_json::to_vec_pretty(profiles).map_err(|error| error.to_string())?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, bytes).map_err(|error| error.to_string())?;
    fs::rename(temp_path, path).map_err(|error| error.to_string())?;
    Ok(())
}
