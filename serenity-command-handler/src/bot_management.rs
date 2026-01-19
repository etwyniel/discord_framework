use anyhow::Context as _;
use futures::{FutureExt, future::BoxFuture};
use serenity::all::{
    AutocompleteChoice, CommandInteraction, Context, CreateAutocompleteResponse, CreateCommand,
    CreateInteractionResponse, GuildId, GuildPagination,
};
use serenity_command::{CommandKey, CommandResponse, args, command};

use crate::command_context::{get_focused_option, get_str_opt_ac};
use crate::prelude::*;

args!(COMMAND_ARGS =
    command[autocomplete]: String,
    guild[autocomplete]: String,
);

const ENABLE_COMMAND: CommandConst = CommandConst {
    description: "Enables a command for a specific Guild (server)",
    is_management: true,
    ..command!(/enable_comand_for_guild COMMAND_ARGS: enable_command_for_guild)
};

async fn enable_command_for_guild(
    (cmd, guild): COMMAND_ARGS,
    handler: &Handler,
    ctx: &Context,
    _command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = guild
        .parse::<u64>()
        .context("Invalid Guild Id, must be an integer")?;
    // check if command exists and get its runner to be able to register it
    let commands = handler.commands.read().await;
    let Some((_, runner)) = commands.0.iter().find(|&((name, _), _)| *name == cmd) else {
        return CommandResponse::private(format!("command {cmd} not found"));
    };
    // save in DB
    let guild = GuildId::new(guild_id);
    handler
        .db
        .lock()
        .await
        .set_command_enabled_for_guild(&cmd, guild, true)?;
    // register command in target guild
    let mut builder = CreateCommand::new(runner.name).description(runner.description);
    builder = (runner.register_options)(builder);
    guild.create_command(&ctx.http, builder).await?;
    CommandResponse::public(format!(
        "Enabled command '{cmd}' for guild with id `{guild_id:?}`"
    ))
}

const DISABLE_COMMAND: CommandConst = CommandConst {
    description: "Disables a command for a specific Guild (server)",
    is_management: true,
    ..command!(/disable_command_for_guild COMMAND_ARGS: disable_command_for_guild)
};

async fn disable_command_for_guild(
    (cmd, guild): COMMAND_ARGS,
    handler: &Handler,
    ctx: &Context,
    _command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = guild
        .parse::<u64>()
        .context("Invalid Guild Id, must be an integer")?;
    // check if command exists and get its runner to be able to register it
    let commands = handler.commands.read().await;
    if !commands.0.iter().any(|(&(name, _), _)| name == cmd) {
        return CommandResponse::private(format!("command {cmd} not found"));
    };
    // save in DB
    let guild = GuildId::new(guild_id);
    handler
        .db
        .lock()
        .await
        .set_command_enabled_for_guild(&cmd, guild, false)?;
    // unregister command in target guild
    for command in guild.get_commands(&ctx.http).await? {
        if command.name != cmd {
            continue;
        }
        guild.delete_command(&ctx.http, command.id).await?;
        break;
    }
    CommandResponse::public(format!(
        "Disabled command '{cmd}' for guild with id `{guild}`",
    ))
}

pub struct ModManagement {}

impl ModManagement {
    fn complete_management_command<'a>(
        handler: &'a Handler,
        ctx: &'a Context,
        _key: CommandKey<'a>,
        ac: &'a CommandInteraction,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        async move {
            let cmd_name = ac.data.name.as_str();
            if cmd_name != ENABLE_COMMAND.name && cmd_name != DISABLE_COMMAND.name {
                return Ok(false);
            }
            let options = &ac.data.options;
            let focused = match get_focused_option(options) {
                Some(opt) => opt,
                None => return Ok(true),
            };
            let partial = get_str_opt_ac(options, focused).unwrap_or_default();
            if focused == "guild" {
                let mut choices = Vec::new();
                let mut last = None;
                while choices.len() < 25 {
                    let target = last.map(GuildPagination::After);
                    let guilds = ctx.http.get_guilds(target, None).await?;
                    last = guilds.last().map(|g| g.id);
                    for g in guilds {
                        if choices.len() < 25 && (partial.is_empty() || g.name.contains(partial)) {
                            choices.push((g.id.to_string(), g.name))
                        }
                    }
                    if last.is_none() {
                        break;
                    }
                }

                let resp = choices
                    .into_iter()
                    .fold(CreateAutocompleteResponse::new(), |resp, (id, name)| {
                        resp.add_choice(AutocompleteChoice::new(name, id))
                    });
                ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(resp))
                    .await?;
            }
            if focused == "command" {
                let commands = handler.commands.read().await;
                let choices: Vec<String> = commands
                    .0
                    .iter()
                    .filter(|((name, _), runner)| {
                        runner.is_guild && (partial.is_empty() || name.contains(partial))
                    })
                    .map(|(&(name, _), _)| name.to_string())
                    .take(25)
                    .collect();

                let resp = choices
                    .into_iter()
                    .fold(CreateAutocompleteResponse::new(), |resp, command| {
                        resp.add_choice(command)
                    });
                ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(resp))
                    .await?;
            }
            Ok(true)
        }
        .boxed()
    }
}

impl RegisterableModule for ModManagement {
    async fn init(_: &crate::ModuleMap) -> anyhow::Result<Self> {
        Ok(Self {})
    }
}

impl Module for ModManagement {
    fn register_commands(
        &self,
        store: &mut CommandStore,
        _modal_store: &mut ModalCommandStore,
        completion_handlers: &mut CompletionStore,
    ) {
        store.register(ENABLE_COMMAND);
        store.register(DISABLE_COMMAND);

        completion_handlers.push(ModManagement::complete_management_command);
    }
}
