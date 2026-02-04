use std::collections::HashMap;

use serenity::all::ComponentInteraction;
use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::futures::future::BoxFuture;
use serenity::model::Permissions;
use serenity::model::application::{CommandInteraction, CommandType, ModalInteraction};
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

pub type CommandFunc<T> = for<'a> fn(
    &'a T,
    &'a Context,
    &'a CommandInteraction,
) -> BoxFuture<'a, anyhow::Result<CommandResponse>>;

pub struct CommandConst<T> {
    pub name: &'static str,
    pub description: &'static str,
    pub run: CommandFunc<T>,
    pub setup_options: fn(&str, CreateCommandOption<'static>) -> CreateCommandOption<'static>,
    pub register_options: fn(CreateCommand<'static>) -> CreateCommand<'static>,
    pub ty: CommandType,
    pub is_guild: bool,
    pub is_management: bool,
    pub permissions: Permissions,
}

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

pub type ModalCommandFunc<T> = for<'a> fn(
    &'a T,
    &'a Context,
    &'a ModalInteraction,
) -> BoxFuture<'a, anyhow::Result<CommandResponse>>;

pub struct ModalCommandConst<T> {
    pub name: &'static str,
    pub run: ModalCommandFunc<T>,
}

pub struct ModalCommandStore<'a, T>(pub HashMap<&'a str, ModalCommandConst<T>>);

impl<T> Default for ModalCommandStore<'_, T> {
    fn default() -> Self {
        ModalCommandStore(HashMap::default())
    }
}

impl<T> ModalCommandStore<'_, T> {
    pub fn register(&mut self, command: ModalCommandConst<T>) {
        self.0.insert(command.name, command);
    }
}

pub type ComponentCommandFunc<T> = for<'a> fn(
    &'a T,
    &'a Context,
    &'a ComponentInteraction,
) -> BoxFuture<'a, anyhow::Result<CommandResponse>>;

pub struct ComponentCommandConst<T> {
    pub name: &'static str,
    pub run: ComponentCommandFunc<T>,
}

pub struct ComponentCommandStore<'a, T>(pub HashMap<&'a str, ComponentCommandConst<T>>);

impl<T> Default for ComponentCommandStore<'_, T> {
    fn default() -> Self {
        ComponentCommandStore(HashMap::default())
    }
}

impl<T> ComponentCommandStore<'_, T> {
    pub fn register(&mut self, command: ComponentCommandConst<T>) {
        self.0.insert(command.name, command);
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

#[macro_export]
macro_rules! modal_command {
    ($name:ident: $f:expr) => {{
        use serenity::futures::FutureExt;
        let f: $crate::ModalCommandFunc<_> =
            |h, c, cmd| async move { ($f)(h, c, cmd).await }.boxed();
        $crate::ModalCommandConst {
            name: stringify!($name),
            run: f,
        }
    }};
    ($name:ident $args:ident: $f:expr) => {{
        use serenity::futures::FutureExt;
        use $crate::ArgList;
        let f: $crate::ModalCommandFunc<_> = |h, c, cmd| {
            async move {
                let args = $args.parse_modal(&cmd.data)?;
                ($f)(args, h, c, cmd).await
            }
            .boxed()
        };
        $crate::ModalCommandConst {
            name: stringify!($name),
            run: f,
        }
    }};
}

#[macro_export]
macro_rules! component_command {
    ($name:ident: $f:expr) => {{
        use serenity::futures::FutureExt;
        let f: $crate::ComponentCommandFunc<_> =
            |h, c, cmd| async move { ($f)(h, c, cmd).await }.boxed();
        $crate::ComponentCommandConst {
            name: stringify!($name),
            run: f,
        }
    }};
    ($name:ident $args:ident: $f:expr) => {{
        use serenity::futures::FutureExt;
        use $crate::ArgList;
        let f: $crate::ComponentCommandFunc<_> = |h, c, cmd| {
            async move {
                let args = $args.parse_component(&cmd.data)?;
                ($f)(args, h, c, cmd).await
            }
            .boxed()
        };
        $crate::ComponentCommandConst {
            name: stringify!($name),
            run: f,
        }
    }};
}
