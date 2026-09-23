#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TableData {
    pub schema: String,
    pub name: String,
    pub offset: u64,
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Default)]
pub struct EditedCell {
    pub value: String,
    pub is_null: bool,
    pub use_default: bool,
}

pub const TABLE_PAGE_SIZE: usize = 100;

pub fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn sql_literal(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("'{}'", value.replace('\'', "''")),
        None => String::from("NULL"),
    }
}

pub fn insert_sql(
    schema: &str,
    table: &str,
    columns: &[ColumnInfo],
    values: &[EditedCell],
) -> String {
    let column_names = columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let value_expressions = values
        .iter()
        .map(|cell| {
            if cell.use_default {
                String::from("DEFAULT")
            } else if cell.is_null {
                String::from("NULL")
            } else {
                sql_literal(Some(&cell.value))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "INSERT INTO {}.{} ({column_names}) VALUES ({value_expressions});",
        quote_identifier(schema),
        quote_identifier(table)
    )
}

pub fn update_sql(
    schema: &str,
    table: &str,
    columns: &[ColumnInfo],
    primary_key: &[String],
    original: &[Option<String>],
    values: &[EditedCell],
) -> Result<String, String> {
    if primary_key.is_empty() {
        return Err(String::from("Cannot update a row without a primary key"));
    }

    let assignments = columns
        .iter()
        .zip(values)
        .map(|(column, cell)| {
            let value = if cell.use_default {
                String::from("DEFAULT")
            } else if cell.is_null {
                String::from("NULL")
            } else {
                sql_literal(Some(&cell.value))
            };
            format!("{} = {value}", quote_identifier(&column.name))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let predicate = primary_key_predicate(columns, primary_key, original)?;

    Ok(format!(
        "UPDATE {}.{} SET {assignments} WHERE {predicate};",
        quote_identifier(schema),
        quote_identifier(table)
    ))
}

pub fn delete_sql(
    schema: &str,
    table: &str,
    columns: &[ColumnInfo],
    primary_key: &[String],
    original: &[Option<String>],
) -> Result<String, String> {
    if primary_key.is_empty() {
        return Err(String::from("Cannot delete a row without a primary key"));
    }

    let predicate = primary_key_predicate(columns, primary_key, original)?;
    Ok(format!(
        "DELETE FROM {}.{} WHERE {predicate};",
        quote_identifier(schema),
        quote_identifier(table)
    ))
}

fn primary_key_predicate(
    columns: &[ColumnInfo],
    primary_key: &[String],
    original: &[Option<String>],
) -> Result<String, String> {
    primary_key
        .iter()
        .map(|key| {
            let index = columns
                .iter()
                .position(|column| &column.name == key)
                .ok_or_else(|| format!("Primary-key column ‘{key}’ is missing"))?;
            let value = original
                .get(index)
                .ok_or_else(|| format!("Original value for primary key ‘{key}’ is missing"))?;
            Ok(format!(
                "{} = {}",
                quote_identifier(key),
                sql_literal(value.as_deref())
            ))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|parts| parts.join(" AND "))
}

#[cfg(test)]
mod tests {
    use super::{ColumnInfo, EditedCell, delete_sql, insert_sql, update_sql};

    #[test]
    fn quotes_identifiers_without_interpreting_sql() {
        assert_eq!(super::quote_identifier("public"), "\"public\"");
        assert_eq!(
            super::quote_identifier("items\"; DROP TABLE users; --"),
            "\"items\"\"; DROP TABLE users; --\""
        );
    }

    fn columns() -> Vec<ColumnInfo> {
        vec![
            ColumnInfo {
                name: String::from("id"),
                data_type: String::from("integer"),
                nullable: false,
                default: None,
            },
            ColumnInfo {
                name: String::from("display name"),
                data_type: String::from("text"),
                nullable: true,
                default: None,
            },
        ]
    }

    #[test]
    fn insert_quotes_identifiers_and_values() {
        let sql = insert_sql(
            "public",
            "users",
            &columns(),
            &[
                EditedCell {
                    value: String::from("7"),
                    ..EditedCell::default()
                },
                EditedCell {
                    value: String::from("O'Brien"),
                    ..EditedCell::default()
                },
            ],
        );

        assert_eq!(
            sql,
            "INSERT INTO \"public\".\"users\" (\"id\", \"display name\") VALUES ('7', 'O''Brien');"
        );
    }

    #[test]
    fn update_and_delete_use_every_primary_key_column() {
        let columns = columns();
        let keys = vec![String::from("id")];
        let original = vec![Some(String::from("7")), Some(String::from("Old"))];
        let updated = vec![
            EditedCell {
                value: String::from("7"),
                ..EditedCell::default()
            },
            EditedCell {
                value: String::from("New"),
                ..EditedCell::default()
            },
        ];

        assert!(
            update_sql("public", "users", &columns, &keys, &original, &updated)
                .expect("update SQL")
                .contains("WHERE \"id\" = '7'")
        );
        assert!(
            delete_sql("public", "users", &columns, &keys, &original)
                .expect("delete SQL")
                .contains("WHERE \"id\" = '7'")
        );
        assert!(update_sql("public", "users", &columns, &[], &original, &updated).is_err());
    }
}
