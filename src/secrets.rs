use keyring::{Entry, Error};
use uuid::Uuid;

const SERVICE_NAME: &str = "com.lucaseufrasio.sql-manager";

pub fn save_password(profile_id: Uuid, password: &str) -> Result<(), Error> {
    Entry::new(SERVICE_NAME, &profile_id.to_string())?.set_password(password)
}

pub fn load_password(profile_id: Uuid) -> Result<Option<String>, Error> {
    match Entry::new(SERVICE_NAME, &profile_id.to_string())?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(Error::NoEntry) => Ok(None),
        Err(error) => Err(error),
    }
}
