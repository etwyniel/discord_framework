use std::collections::HashMap;

use serenity::async_trait;
use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::futures::future::BoxFuture;
use serenity::model::Permissions;
use serenity::model::application::{
    CommandData, CommandInteraction, CommandType, ModalInteraction,
};
use serenity::model::prelude::GuildId;
use serenity::prelude::Context;

mod command_response;
pub use command_response::*;

mod command_data;
pub use command_data::*;

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

    fn setup_options(
        _opt_name: &'static str,
        opt: CreateCommandOption<'static>,
    ) -> CreateCommandOption<'static> {
        opt
    }

    const PERMISSIONS: Permissions = Permissions::empty();
    const GUILD: Option<GuildId> = None;
    const GUILD_COMMAND: bool = false;
    const IS_MANAGEMENT_COMMAND: bool = false;
}

pub trait CommandBuilder<'a>: BotCommand + From<&'a CommandData> + 'static {
    fn create_extras<
        E: Fn(&'static str, CreateCommandOption<'static>) -> CreateCommandOption<'static>,
    >(
        builder: CreateCommand<'static>,
        extras: E,
    ) -> CreateCommand<'static>;
    fn create(builder: CreateCommand<'static>) -> CreateCommand<'static>;
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
    fn register(&self) -> CreateCommand<'static>;

    fn guild(&self) -> Option<GuildId> {
        None
    }

    fn is_guild_command(&self) -> bool {
        false
    }

    fn is_management(&self) -> bool {
        false
    }
}

#[async_trait]
pub trait ModalCommandRunner<T> {
    async fn run(
        &self,
        data: &T,
        ctx: &Context,
        interaction: &ModalInteraction,
    ) -> anyhow::Result<CommandResponse>;
    fn name(&self) -> &'static str;
}

pub struct ModalCommandStore<'a, T>(
    pub HashMap<&'a str, Box<dyn ModalCommandRunner<T> + Send + Sync>>,
);

impl<T> Default for ModalCommandStore<'_, T> {
    fn default() -> Self {
        ModalCommandStore(HashMap::default())
    }
}

impl<T> ModalCommandStore<'_, T> {
    pub fn register<B: ModalCommandRunner<T> + Send + Sync + 'static>(&mut self, runner: B) {
        self.0.insert(runner.name(), Box::new(runner));
    }
}

pub struct CommandConst<T> {
    pub name: &'static str,
    pub description: &'static str,
    pub func: for<'a> fn(
        &'a T,
        &'a Context,
        &'a CommandInteraction,
    ) -> BoxFuture<'a, anyhow::Result<CommandResponse>>,
}
