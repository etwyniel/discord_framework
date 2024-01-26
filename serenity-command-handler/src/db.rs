use anyhow;
use rusqlite::{
    params,
    types::{FromSql, ValueRef},
    Connection, ToSql,
};

use std::borrow::Cow;

use crate::Handler;

pub struct Db {
    pub conn: Connection,
}

impl Db {
    pub fn get_guild_field<T: FromSql + Default>(
        &mut self,
        guild_id: u64,
        field: &str,
    ) -> anyhow::Result<T> {
        match self.conn.query_row(
            &format!("SELECT {field} FROM guild WHERE id = ?1"),
            [guild_id],
            |row| row.get(0),
        ) {
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Default::default()),
            res => res,
        }
        .map_err(anyhow::Error::from)
    }

    pub fn set_guild_field<T: ToSql>(
        &mut self,
        guild_id: u64,
        field: &str,
        value: T,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            &format!("UPDATE guild SET {field} = ?2 WHERE id = ?1"),
            params![guild_id, value],
        )?;
        Ok(())
    }

    pub fn add_guild_field(&mut self, name: &str, def: &str) -> anyhow::Result<()> {
        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS guild(id INTEGER PRIMARY KEY)",
                [],
            )
            .map_err(anyhow::Error::from)?;
        let count: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('guild') WHERE name = ?1",
            [name],
            |row| row.get(0),
        )?;
        if count != 0 {
            return Ok(());
        }
        self.conn
            .execute(&format!("ALTER TABLE guild ADD COLUMN {name} {def}"), [])
            .map_err(anyhow::Error::from)?;
        Ok(())
    }
}

pub fn escape_str(s: &str) -> Cow<'_, str> {
    if !s.contains('\'') {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace('\'', "''"))
}

pub fn column_as_string(val: ValueRef<'_>) -> rusqlite::Result<String> {
    Ok(match val {
        ValueRef::Null => String::new(),
        ValueRef::Real(r) => r.to_string(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Text(b) | ValueRef::Blob(b) => std::str::from_utf8(b)
            .map_err(rusqlite::Error::Utf8Error)?
            .to_string(),
    })
}

impl Handler {
    pub async fn get_guild_field<T: FromSql + Default>(
        &self,
        guild_id: u64,
        field: &str,
    ) -> anyhow::Result<T> {
        self.db.lock().await.get_guild_field(guild_id, field)
    }

    pub async fn set_guild_field<T: ToSql>(
        &self,
        guild_id: u64,
        field: &str,
        value: T,
    ) -> anyhow::Result<()> {
        self.db.lock().await.set_guild_field(guild_id, field, value)
    }
}
