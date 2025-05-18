use anyhow::Context as _;
use futures::{future::BoxFuture, FutureExt};
use serenity::{
    all::{
        CommandInteraction, Context, CreateAutocompleteResponse, CreateInteractionResponse,
        GuildId, GuildPagination, Permissions,
    },
    async_trait,
};
use serenity_command::{BotCommand, CommandBuilder, CommandKey, CommandResponse};
use serenity_command_derive::Command;

use crate::{
    command_context::{get_focused_option, get_str_opt_ac},
    CommandStore, CompletionStore, Handler, Module, RegisterableModule,
};

#[derive(Command)]
#[cmd(
    name = "enable_command_for_guild",
    desc = "Enables a command for a specific Guild (server)"
)]
pub struct EnableCommandForGuild {
    #[cmd(autocomplete)]
    pub command: String,
    #[cmd(autocomplete)]
    pub guild: String,
}

#[async_trait]
impl BotCommand for EnableCommandForGuild {
    type Data = Handler;
    const IS_MANAGEMENT_COMMAND: bool = true;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        _: &CommandInteraction,
    ) -> anyhow::Result<serenity_command::CommandResponse> {
        let guild_id = self
            .guild
            .parse::<u64>()
            .context("Invalid Guild Id, must be an integer")?;
        // check if command exists and get its runner to be able to register it
        let commands = handler.commands.read().await;
        let Some((_, runner)) = commands
            .0
            .iter()
            .find(|(&(name, _), _)| name == self.command)
        else {
            return CommandResponse::private(format!("command {} not found", self.command));
        };
        // save in DB
        let guild = GuildId::new(guild_id);
        handler
            .db
            .lock()
            .await
            .set_command_enabled_for_guild(&self.command, guild, true)?;
        // register command in target guild
        guild.create_command(&ctx, runner.register()).await?;
        CommandResponse::public(format!(
            "Enabled command '{}' for guild with id `{}`",
            &self.command, self.guild
        ))
    }
}

#[derive(Command)]
#[cmd(
    name = "disable_command_for_guild",
    desc = "Disables a command for a specific Guild (server)"
)]
pub struct DisableCommandForGuild {
    #[cmd(autocomplete)]
    pub command: String,
    #[cmd(autocomplete)]
    pub guild: String,
}

#[async_trait]
impl BotCommand for DisableCommandForGuild {
    type Data = Handler;
    const IS_MANAGEMENT_COMMAND: bool = true;
    const PERMISSIONS: Permissions = Permissions::ADMINISTRATOR;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        _: &CommandInteraction,
    ) -> anyhow::Result<serenity_command::CommandResponse> {
        let guild_id = self
            .guild
            .parse::<u64>()
            .context("Invalid Guild Id, must be an integer")?;
        // check if command exists and get its runner to be able to register it
        let commands = handler.commands.read().await;
        if !commands
            .0
            .iter()
            .any(|(&(name, _), _)| name == self.command)
        {
            return CommandResponse::private(format!("command {} not found", self.command));
        };
        // save in DB
        let guild = GuildId::new(guild_id);
        handler
            .db
            .lock()
            .await
            .set_command_enabled_for_guild(&self.command, guild, false)?;
        // unregister command in target guild
        for command in guild.get_commands(&ctx).await? {
            if &command.name != &self.command {
                continue;
            }
            guild.delete_command(&ctx, command.id).await?;
            break;
        }
        CommandResponse::public(format!(
            "Disabled command '{}' for guild with id `{}`",
            &self.command, self.guild
        ))
    }
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
            if cmd_name != EnableCommandForGuild::NAME && cmd_name != DisableCommandForGuild::NAME {
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
                        resp.add_string_choice(name, id)
                    });
                ac.create_response(&ctx, CreateInteractionResponse::Autocomplete(resp))
                    .await?;
            }
            if focused == "command" {
                let commands = handler.commands.read().await;
                let choices: Vec<String> = commands
                    .0
                    .iter()
                    .filter(|((name, _), runner)| {
                        runner.is_guild_command() && (partial.is_empty() || name.contains(partial))
                    })
                    .map(|(&(name, _), _)| name.to_string())
                    .take(25)
                    .collect();

                let resp = choices
                    .into_iter()
                    .fold(CreateAutocompleteResponse::new(), |resp, command| {
                        resp.add_string_choice(command.clone(), command)
                    });
                ac.create_response(&ctx, CreateInteractionResponse::Autocomplete(resp))
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
        completion_handlers: &mut CompletionStore,
    ) {
        store.register::<EnableCommandForGuild>();
        store.register::<DisableCommandForGuild>();

        completion_handlers.push(ModManagement::complete_management_command);
    }
}
