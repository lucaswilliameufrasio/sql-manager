use std::{future::Future, pin::Pin};

use crate::{
    connection::{ConnectionProfile, test_connection},
    database::{Event, PostgresSession},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    #[default]
    #[serde(rename = "postgresql")]
    PostgreSql,
}

impl EngineKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::PostgreSql => "PostgreSQL",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::PostgreSql => 5432,
        }
    }
}

pub trait DatabaseSession: Send {
    fn list_schemas(&self) -> Result<(), String>;
    fn list_tables(&self, schema: String) -> Result<(), String>;
    fn execute(&self, sql: String) -> Result<(), String>;
    fn load_table(&self, schema: String, table: String, offset: u64) -> Result<(), String>;
    fn drain_events(&self) -> Vec<Event>;
}

pub trait EngineAdapter: Send + Sync {
    fn connect(&self, profile: ConnectionProfile, password: String) -> Box<dyn DatabaseSession>;
    fn test_connection<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}

struct PostgreSqlAdapter;

impl EngineAdapter for PostgreSqlAdapter {
    fn connect(&self, profile: ConnectionProfile, password: String) -> Box<dyn DatabaseSession> {
        Box::new(PostgresSession::connect(profile, password))
    }

    fn test_connection<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(test_connection(profile, password))
    }
}

static POSTGRESQL_ADAPTER: PostgreSqlAdapter = PostgreSqlAdapter;

pub fn adapter(kind: EngineKind) -> &'static dyn EngineAdapter {
    match kind {
        EngineKind::PostgreSql => &POSTGRESQL_ADAPTER,
    }
}

#[cfg(test)]
mod tests {
    use super::{EngineKind, adapter};

    #[test]
    fn postgres_engine_has_a_stable_profile_identifier_and_default_port() {
        assert_eq!(EngineKind::PostgreSql.default_port(), 5432);
        let _ = adapter(EngineKind::PostgreSql);
        assert_eq!(
            serde_json::to_string(&EngineKind::PostgreSql).expect("serialize engine"),
            "\"postgresql\""
        );
    }
}
