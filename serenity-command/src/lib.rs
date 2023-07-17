use serenity::async_trait;
use serenity::builder::{CreateApplicationCommand, CreateApplicationCommandOption, CreateEmbed};
use serenity::model::application::interaction::application_command::{
    ApplicationCommandInteraction, CommandData,
};
use serenity::model::prelude::command::CommandType;
use serenity::model::prelude::interaction::MessageFlags;
use serenity::model::prelude::GuildId;
use serenity::model::Permissions;
use serenity::prelude::Context;

#[derive(Debug)]
pub enum CommandResponse {
    None,
    Public(String),
    Private(String),
    Embed(CreateEmbed),
}

pub type CommandKey<'a> = (&'a str, CommandType);

#[async_trait]
pub trait BotCommand {
    type Data;
    async fn run(
        self,
        data: &Self::Data,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse>;

    fn setup_options(_opt_name: &'static str, _opt: &mut CreateApplicationCommandOption) {}

    const PERMISSIONS: Permissions = Permissions::empty();
    const GUILD: Option<GuildId> = None;
}

impl CommandResponse {
    pub fn to_contents_and_flags(self) -> Option<(String, Option<CreateEmbed>, MessageFlags)> {
        Some(match self {
            CommandResponse::None => return None,
            CommandResponse::Public(s) => (s, None, MessageFlags::empty()),
            CommandResponse::Private(s) => (s, None, MessageFlags::EPHEMERAL),
            CommandResponse::Embed(e) => (String::new(), Some(e), MessageFlags::empty()),
        })
    }
}

pub trait CommandBuilder<'a>: BotCommand + From<&'a CommandData> + 'static {
    fn create_extras<E: Fn(&'static str, &mut CreateApplicationCommandOption)>(
        builder: &mut CreateApplicationCommand,
        extras: E,
    ) -> &mut CreateApplicationCommand;
    fn create(builder: &mut CreateApplicationCommand) -> &mut CreateApplicationCommand;
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
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse>;
    fn name(&self) -> CommandKey<'static>;
    fn register<'a>(
        &self,
        builder: &'a mut CreateApplicationCommand,
    ) -> &'a mut CreateApplicationCommand;

    fn guild(&self) -> Option<GuildId> {
        None
    }
}
