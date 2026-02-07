use anyhow::Context as _;
use serenity::all::{CommandInteraction, Context, Permissions, RoleId};

use crate::prelude::*;

args!(SETCREATETHREADS_ARGS = create_threads: bool);

pub const SETCREATETHREADS: CommandConst = CommandConst {
    description: "Configure thread creation for listening parties",
    permissions: Permissions::MANAGE_THREADS,
    ..command!(/setcreatethreads SETCREATETHREADS_ARGS: set_create_threads)
};

async fn set_create_threads(
    (create_threads,): SETCREATETHREADS_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let db = handler.db.lock().await;
    db.set_guild_field(guild_id, "create_threads", create_threads)
        .context("updating 'create_threads' guild field")?;
    let resp = if create_threads {
        "Will create threads when setting up listening parties"
    } else {
        "Will not create threads when setting up listening parties"
    };
    CommandResponse::private(resp)
}

args!(SETROLE_ARGS = role: Option<RoleId>);

pub const SETROLE: CommandConst = CommandConst {
    description: "Set the role to ping for Listening Parties",
    permissions: Permissions::MANAGE_ROLES,
    ..command!(/setrole SETROLE_ARGS: setrole)
};

async fn setrole(
    (role,): SETROLE_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let role = role.as_ref().map(|r| r.get().to_string());
    let db = handler.db.lock().await;
    db.set_guild_field(guild_id, "role_id", &role)
        .context("updating 'role_id' guild field")?;
    let resp = if let Some(role_id) = role {
        format!("Set listening party role to <@&{role_id}>.")
    } else {
        "Unset listening party role.".to_string()
    };
    CommandResponse::private(resp)
}

args!(SETWEBHOOK_ARGS =
    webhook: Option<String>,
);

pub const SETWEBHOOK: CommandConst = CommandConst {
    description: "set a webhook to use when creating listening parties",
    permissions: Permissions::MANAGE_WEBHOOKS,
    ..command!(/setwebhook SETWEBHOOK_ARGS: setwebhook)
};

async fn setwebhook(
    (webhook,): SETWEBHOOK_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let db = handler.db.lock().await;
    db.set_guild_field(guild_id, "webhook", webhook.as_ref())
        .context("updating 'webhook' guild field")?;
    let resp = if webhook.is_some() {
        "Listening parties will be created using a webhook."
    } else {
        "Listening parties will not be created using a webhook."
    };
    CommandResponse::private(resp)
}
