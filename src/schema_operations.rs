use crate::schema::quote_identifier;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnType {
    #[default]
    Text,
    Integer,
    BigInt,
    Numeric,
    Boolean,
    Date,
    TimestampTz,
    Uuid,
    Jsonb,
}

impl ColumnType {
    pub const ALL: [Self; 9] = [
        Self::Text,
        Self::Integer,
        Self::BigInt,
        Self::Numeric,
        Self::Boolean,
        Self::Date,
        Self::TimestampTz,
        Self::Uuid,
        Self::Jsonb,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Integer => "Integer",
            Self::BigInt => "Big integer",
            Self::Numeric => "Numeric",
            Self::Boolean => "Boolean",
            Self::Date => "Date",
            Self::TimestampTz => "Timestamp with time zone",
            Self::Uuid => "UUID",
            Self::Jsonb => "JSONB",
        }
    }

    fn sql(self) -> &'static str {
        match self {
            Self::Text => "TEXT",
            Self::Integer => "INTEGER",
            Self::BigInt => "BIGINT",
            Self::Numeric => "NUMERIC",
            Self::Boolean => "BOOLEAN",
            Self::Date => "DATE",
            Self::TimestampTz => "TIMESTAMPTZ",
            Self::Uuid => "UUID",
            Self::Jsonb => "JSONB",
        }
    }
}

#[derive(Clone, Debug)]
pub struct NewColumn {
    pub name: String,
    pub kind: ColumnType,
    pub nullable: bool,
    pub primary_key: bool,
}

pub fn create_table_sql(
    schema: &str,
    table: &str,
    columns: &[NewColumn],
) -> Result<String, String> {
    validate_identifier(table, "Table name")?;
    if columns.is_empty() {
        return Err(String::from("A table must have at least one column"));
    }

    let mut definitions = Vec::with_capacity(columns.len() + 1);
    let mut primary_keys = Vec::new();
    let mut column_names = HashSet::with_capacity(columns.len());
    for column in columns {
        validate_identifier(&column.name, "Column name")?;
        if !column_names.insert(&column.name) {
            return Err(format!(
                "Column ‘{}’ is defined more than once",
                column.name
            ));
        }
        let nullability = if column.nullable && !column.primary_key {
            ""
        } else {
            " NOT NULL"
        };
        definitions.push(format!(
            "{} {}{nullability}",
            quote_identifier(&column.name),
            column.kind.sql()
        ));
        if column.primary_key {
            primary_keys.push(quote_identifier(&column.name));
        }
    }

    if !primary_keys.is_empty() {
        definitions.push(format!("PRIMARY KEY ({})", primary_keys.join(", ")));
    }

    Ok(format!(
        "CREATE TABLE {}.{} ({});",
        quote_identifier(schema),
        quote_identifier(table),
        definitions.join(", ")
    ))
}

pub fn add_column_sql(schema: &str, table: &str, column: &NewColumn) -> Result<String, String> {
    validate_identifier(&column.name, "Column name")?;
    let nullability = if column.nullable { "" } else { " NOT NULL" };
    Ok(format!(
        "ALTER TABLE {}.{} ADD COLUMN {} {}{nullability};",
        quote_identifier(schema),
        quote_identifier(table),
        quote_identifier(&column.name),
        column.kind.sql()
    ))
}

pub fn rename_table_sql(schema: &str, table: &str, new_name: &str) -> Result<String, String> {
    validate_identifier(new_name, "New table name")?;
    Ok(format!(
        "ALTER TABLE {}.{} RENAME TO {};",
        quote_identifier(schema),
        quote_identifier(table),
        quote_identifier(new_name)
    ))
}

pub fn drop_table_sql(schema: &str, table: &str) -> String {
    format!(
        "DROP TABLE {}.{} RESTRICT;",
        quote_identifier(schema),
        quote_identifier(table)
    )
}

pub fn rename_column_sql(
    schema: &str,
    table: &str,
    column: &str,
    new_name: &str,
) -> Result<String, String> {
    validate_identifier(new_name, "New column name")?;
    Ok(format!(
        "ALTER TABLE {}.{} RENAME COLUMN {} TO {};",
        quote_identifier(schema),
        quote_identifier(table),
        quote_identifier(column),
        quote_identifier(new_name)
    ))
}

pub fn drop_column_sql(schema: &str, table: &str, column: &str) -> String {
    format!(
        "ALTER TABLE {}.{} DROP COLUMN {} RESTRICT;",
        quote_identifier(schema),
        quote_identifier(table),
        quote_identifier(column)
    )
}

fn validate_identifier(identifier: &str, label: &str) -> Result<(), String> {
    if identifier.trim().is_empty() {
        Err(format!("{label} cannot be empty"))
    } else if identifier.len() > 63 {
        Err(format!(
            "{label} must be at most 63 characters in PostgreSQL"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ColumnType, NewColumn, add_column_sql, create_table_sql, drop_column_sql, drop_table_sql,
        rename_column_sql, rename_table_sql,
    };

    #[test]
    fn creates_table_using_a_type_allowlist_and_quoted_identifiers() {
        let sql = create_table_sql(
            "public",
            "user data",
            &[
                NewColumn {
                    name: String::from("id"),
                    kind: ColumnType::BigInt,
                    nullable: false,
                    primary_key: true,
                },
                NewColumn {
                    name: String::from("full name"),
                    kind: ColumnType::Text,
                    nullable: true,
                    primary_key: false,
                },
            ],
        )
        .expect("create table SQL");

        assert_eq!(
            sql,
            "CREATE TABLE \"public\".\"user data\" (\"id\" BIGINT NOT NULL, \"full name\" TEXT, PRIMARY KEY (\"id\"));"
        );
    }

    #[test]
    fn emits_safe_non_cascading_schema_operations() {
        assert!(drop_table_sql("public", "users").ends_with("RESTRICT;"));
        assert!(drop_column_sql("public", "users", "email").ends_with("RESTRICT;"));
        assert!(rename_table_sql("public", "users", "people").is_ok());
        assert!(rename_column_sql("public", "users", "email", "address").is_ok());
        assert!(
            add_column_sql(
                "public",
                "users",
                &NewColumn {
                    name: String::from("active"),
                    kind: ColumnType::Boolean,
                    nullable: false,
                    primary_key: false,
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_empty_and_overlong_identifiers() {
        assert!(create_table_sql("public", "", &[]).is_err());
        assert!(rename_table_sql("public", "users", &"x".repeat(64)).is_err());
    }
}
