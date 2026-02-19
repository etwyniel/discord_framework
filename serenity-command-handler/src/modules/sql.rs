use anyhow::{Context as _, anyhow, bail};
use itertools::Itertools;
use rusqlite::{Connection, types::ValueRef};
use serenity::{
    async_trait,
    model::{Permissions, prelude::CommandInteraction, prelude::UserId},
    prelude::Context,
};
use serenity_command::{CommandResponse, args, command};
use tokio::task::block_in_place;

use crate::{db::Db, prelude::*};

args!(QUERY_ARGS =
    qry: String,
);

pub const QUERY: CommandConst = CommandConst {
    description: "Query the database (admin-only)",
    permissions: Permissions::MANAGE_GUILD,
    ..command!(/query QUERY_ARGS: query)
};

/// Respond with the result of an SQL query.
///
/// This command can only be called by admins.
pub fn do_query(
    mut qry: &str,
    db: &Connection,
    requester: UserId,
    repeat_query: bool,
) -> anyhow::Result<CommandResponse> {
    qry = qry
        .trim_start_matches("```")
        .trim_start_matches("sql")
        .trim_end_matches("```")
        .trim();
    let qry_context = if repeat_query {
        format!("```sql\n{qry}```")
    } else {
        String::new()
    };
    // check user is admin
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
    let result: Vec<Vec<_>> = block_in_place(|| {
        stmt.query_map([], |row| {
            // format results
            let mut result = Vec::with_capacity(n_columns);
            for i in 0..n_columns {
                let value = match row.get_ref(i)? {
                    ValueRef::Null => None,
                    ValueRef::Integer(n) => Some(n.to_string()),
                    ValueRef::Real(r) => Some(r.to_string()),
                    ValueRef::Text(t) => Some(String::from_utf8_lossy(t).to_string()),
                    ValueRef::Blob(_) => Some("<binary data>".to_string()),
                };
                result.push(value)
            }
            Ok(result)
        })?
        .take(10)
        .collect::<Result<_, _>>()
    })
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

/// Respond with the result of an SQL query.
///
/// This command can only be called by admins.
pub async fn query(
    (query,): QUERY_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let db = handler.db.lock().await;
    do_query(&query, db.conn(), command.user.id, true)
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

    fn register_commands(&self, store: &mut dyn Storer) {
        store.register(QUERY);
    }
}

impl RegisterableModule for Sql {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Sql)
    }
}
