use anyhow;
use rusqlite::{
    params,
    types::{FromSql, ValueRef},
    Connection, ToSql,
};
use serenity::all::GuildId;

use std::borrow::Cow;

use crate::Handler;

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn new(conn: Connection) -> anyhow::Result<Db> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS guild(id INTEGER PRIMARY KEY)",
            [],
        )
        .map_err(anyhow::Error::from)?;

        conn.execute("CREATE TABLE IF NOT EXISTS enabled_guild_commands (guild_id INTEGER, command_name STRING)", [])
            .map_err(anyhow::Error::from)?;

        Ok(Db { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn get_guild_field<T: FromSql + Default>(
        &self,
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
        &self,
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

    pub fn set_command_enabled_for_guild(
        &mut self,
        command_name: &str,
        guild_id: GuildId,
        enable: bool,
    ) -> anyhow::Result<()> {
        if enable {
            self.conn
                .execute(
                    "INSERT INTO enabled_guild_commands (guild_id, command_name) VALUES  (?1, ?2)",
                    params![guild_id.get(), command_name],
                )
                .map_err(anyhow::Error::from)
        } else {
            self.conn
                .execute(
                    "DELETE FROM enabled_guild_commands WHERE guild_id = ?1 AND command_name = ?2",
                    params![guild_id.get(), command_name],
                )
                .map_err(anyhow::Error::from)
        }?;
        Ok(())
    }

    pub fn get_command_enabled_guilds(&mut self, command_name: &str) -> Vec<GuildId> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT guild_id FROM enabled_guild_commands WHERE command_name = ?1")
        else {
            return vec![];
        };
        let Ok(res) = stmt.query_map([command_name], |row| Ok(GuildId::new(row.get(0)?))) else {
            return vec![];
        };
        res.filter_map(|row| row.ok()).collect()
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
