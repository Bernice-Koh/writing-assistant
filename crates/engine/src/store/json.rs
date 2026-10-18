//! A column that holds a value as its serde JSON: a clause's register scope, a checker's definition
//! and its provenance.

use std::any::type_name;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// A value bound to or read from a column as its serde JSON. A failure names the value's type and
/// where in the JSON it failed, never the JSON: serde's own message can quote a register name or a
/// checker's phrase, and every error of the store module leaves a row's content out.
pub(super) struct Json<T>(pub(super) T);

impl<T: Serialize> ToSql for Json<T> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        serde_json::to_string(&self.0)
            .map(ToSqlOutput::from)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(described::<T>(&error).into()))
    }
}

impl<T: DeserializeOwned> FromSql for Json<T> {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        serde_json::from_str(value.as_str()?)
            .map(Json)
            .map_err(|error| FromSqlError::Other(described::<T>(&error).into()))
    }
}

fn described<T>(error: &serde_json::Error) -> String {
    format!(
        "the JSON of a {} failed with a {:?} error at line {}, column {}",
        type_name::<T>(),
        error.classify(),
        error.line(),
        error.column()
    )
}
