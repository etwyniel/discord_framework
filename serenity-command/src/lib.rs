use std::collections::HashMap;

use serenity::async_trait;
use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::model::application::{CommandData, CommandInteraction, CommandType};
use serenity::model::prelude::GuildId;
use serenity::model::Permissions;
use serenity::prelude::Context;

mod command_response;
pub use command_response::*;

pub type CommandKey<'a> = (&'a str, CommandType);

pub struct CommandStore<'a, T>(
    pub HashMap<CommandKey<'a>, Box<dyn CommandRunner<T> + Send + Sync>>,
);

impl<T> Default for CommandStore<'_, T> {
    fn default() -> Self {
        CommandStore(HashMap::default())
    }
}

impl<T> CommandStore<'_, T> {
    pub fn register<B: CommandBuilder<'static, Data = T>>(&mut self) {
        let runner = B::runner();
        self.0.insert(runner.name(), runner);
    }
}

#[async_trait]
pub trait BotCommand {
    type Data;
    async fn run(
        self,
        data: &Self::Data,
        ctx: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse>;

    fn setup_options(_opt_name: &'static str, opt: CreateCommandOption) -> CreateCommandOption {
        opt
    }

    const PERMISSIONS: Permissions = Permissions::empty();
    const GUILD: Option<GuildId> = None;
}

pub trait CommandBuilder<'a>: BotCommand + From<&'a CommandData> + 'static {
    fn create_extras<E: Fn(&'static str, CreateCommandOption) -> CreateCommandOption>(
        builder: CreateCommand,
        extras: E,
    ) -> CreateCommand;
    fn create(builder: CreateCommand) -> CreateCommand;
    const NAME: &'static str;
    const TYPE: CommandType = CommandType::ChatInput;
    fn runner() -> Box<dyn CommandRunner<Self::Data> + Send + Sync>;
}

#[async_trait]
pub trait CommandRunner<T> {
    async fn run(
        &self,
        data: &T,
        ctx: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse>;
    fn name(&self) -> CommandKey<'static>;
    fn register(&self) -> CreateCommand;

    fn guild(&self) -> Option<GuildId> {
        None
    }
}
