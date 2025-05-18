use anyhow::{anyhow, bail, Context as _};
use itertools::Itertools;
use rusqlite::{types::ValueRef, Connection};
use serenity::{
    async_trait,
    model::{prelude::CommandInteraction, prelude::UserId, Permissions},
    prelude::Context,
};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;

use crate::{
    db::Db, CommandStore, CompletionStore, Handler, Module, ModuleMap, RegisterableModule,
};

#[derive(Command)]
#[cmd(name = "query", desc = "Query the database (admin-only)")]
pub struct Query {
    pub qry: String,
}

impl Query {
    pub fn query(
        &self,
        db: &Connection,
        requester: UserId,
        repeat_query: bool,
    ) -> anyhow::Result<CommandResponse> {
        let qry = self
            .qry
            .trim_start_matches("```")
            .trim_start_matches("sql")
            .trim_end_matches("```")
            .trim();
        let qry_context = if repeat_query {
            format!("```sql\n{qry}```")
        } else {
            String::new()
        };
        // check user is amin
        match db.query_row(
            "SELECT id FROM admin WHERE id = ?1",
            [requester.get()],
            |row| row.get::<_, u64>(0),
        ) {
            Ok(_) => (),
            Err(rusqlite::Error::QueryReturnedNoRows) => bail!("Admin-only command"),
            err @ Err(_) => return err.context(qry_context).map(|_| CommandResponse::None),
        }
        let mut stmt = db.prepare(qry)?;
        let n_columns = stmt.column_count();
        let result: Vec<Vec<_>> = stmt
            .query_map([], |row| {
                let mut result = Vec::with_capacity(n_columns);
                for i in 0..n_columns {
                    let value = match row.get_ref(i) {
                        Ok(ValueRef::Null) => None,
                        Ok(ValueRef::Integer(n)) => Some(n.to_string()),
                        Ok(ValueRef::Real(r)) => Some(r.to_string()),
                        Ok(ValueRef::Text(t)) => Some(String::from_utf8_lossy(t).to_string()),
                        Ok(ValueRef::Blob(_)) => Some("<binary data>".to_string()),
                        Err(e) => return Err(e),
                    };
                    result.push(value)
                }
                Ok(result)
            })?
            .take(10)
            .collect::<Result<_, _>>()
            .map_err(|e| anyhow!("{qry_context}{e}"))?;
        let mut resp = format!("{qry_context}```\n");
        resp.push_str(&stmt.column_names().join("|"));
        for row in result {
            resp.push('\n');
            resp.push_str(
                &row.iter()
                    .map(|val| val.as_deref().unwrap_or("NULL"))
                    .join("|"),
            );
        }
        resp.push_str("```");
        CommandResponse::public(resp)
    }
}

#[async_trait]
impl BotCommand for Query {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_GUILD;

    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        cmd: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let db = handler.db.lock().await;
        self.query(db.conn(), cmd.user.id, true)
    }
}

pub struct Sql;

#[async_trait]
impl Module for Sql {
    async fn setup(&mut self, db: &mut Db) -> anyhow::Result<()> {
        db.conn().execute(
            "CREATE TABLE IF NOT EXISTS admin (id INTEGER PRIMARY KEY)",
            [],
        )?;
        Ok(())
    }

    fn register_commands(&self, store: &mut CommandStore, _completions: &mut CompletionStore) {
        store.register::<Query>();
    }
}

impl RegisterableModule for Sql {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Sql)
    }
}
