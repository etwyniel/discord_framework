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

pub struct CommandStore<'a, T>(pub HashMap<CommandKey<'a>, CommandConst<T>>);

impl<T> Default for CommandStore<'_, T> {
    fn default() -> Self {
        CommandStore(HashMap::default())
    }
}

impl<T> CommandStore<'_, T> {
    pub fn register(&mut self, command: CommandConst<T>) {
        let key = (command.name, command.ty);
        self.0.insert(key, command);
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

// pub type CommandFunc<'a, T, F> = fn(&'a T, &'a Context, &'a CommandInteraction) -> F;
pub type CommandFunc<T> = for<'a> fn(
    &'a T,
    &'a Context,
    &'a CommandInteraction,
) -> BoxFuture<'a, anyhow::Result<CommandResponse>>;

pub struct CommandConst<T> {
    pub name: &'static str,
    pub description: &'static str,
    // pub run: Box<
    //     dyn for<'a> Fn(
    //         &'a T,
    //         &'a Context,
    //         &'a CommandInteraction,
    //     ) -> BoxFuture<'a, anyhow::Result<CommandResponse>>,
    // >,
    pub run: CommandFunc<T>,
    pub setup_options: fn(&str, CreateCommandOption<'static>) -> CreateCommandOption<'static>,
    pub register_options: fn(CreateCommand<'static>) -> CreateCommand<'static>,
    pub ty: CommandType,
    pub is_guild: bool,
    pub is_management: bool,
    pub permissions: Permissions,
}

// pub const fn command_default<T, F>(name: &'static str, run: CommandFunc<T, F>) -> CommandConst<T>
// where
//     F: Future<Output = anyhow::Result<CommandResponse>>,
// {
//     CommandConst {
//         name,
//         description: "",
//         run: Box::new(|h, c, cmd| async move { run(h, c, cmd).await }.boxed()),
//         setup_options: |_, o| o,
//         register_options: |c| c,
//         ty: CommandType::ChatInput,
//         is_guild: false,
//         is_management: false,
//     }
// }

pub const fn command_default<T>(name: &'static str, run: CommandFunc<T>) -> CommandConst<T> {
    CommandConst {
        name,
        description: "",
        run,
        setup_options: |_, o| o,
        register_options: |c| c,
        ty: CommandType::ChatInput,
        is_guild: false,
        is_management: false,
        permissions: Permissions::empty(),
    }
}

#[macro_export]
macro_rules! command {
    (/$name:ident: $f:expr) => {{
        use serenity::futures::FutureExt;
        let f: $crate::CommandFunc<_> = |h, c, cmd| async move { ($f)(h, c, cmd).await }.boxed();
        $crate::command_default(stringify!($name), f)
    }};
    (/$name:ident $args:ident: $f:expr) => {{
        use serenity::futures::FutureExt;
        use $crate::ArgList;
        let f: $crate::CommandFunc<_> = |h, c, cmd| {
            async move {
                let args = $args.parse(&cmd.data)?;
                ($f)(args, h, c, cmd).await
            }
            .boxed()
        };
        $crate::CommandConst {
            register_options: |cmd| $args.add_options(cmd),
            ..$crate::command_default(stringify!($name), f)
        }
    }};
    (/$name:ident $args:ident($extra:ident): $f:expr) => {{
        use serenity::futures::FutureExt;
        use $crate::ArgList;
        let f: $crate::CommandFunc<_> = |h, c, cmd| {
            async move {
                let args = $args.parse(&cmd.data)?;
                ($f)(args, h, c, cmd).await
            }
            .boxed()
        };
        $crate::CommandConst {
            register_options: |cmd| $args.add_options_with(cmd, $extra),
            ..$crate::command_default(stringify!($name), f)
        }
    }};
    (/$name:ident(Message): $f:expr) => {{
        use serenity::futures::FutureExt;
        let f: $crate::CommandFunc<_> = |h, c, cmd| {
            async move {
                use anyhow::Context;
                let msg = cmd
                    .data
                    .resolved
                    .messages
                    .iter()
                    .next()
                    .context("missing message for message command")?;
                ($f)(msg, h, c, cmd).await
            }
            .boxed()
        };
        $crate::CommandConst {
            ty: serenity::model::application::CommandType::Message,
            ..$crate::command_default(stringify!($name), f)
        }
    }};
}
