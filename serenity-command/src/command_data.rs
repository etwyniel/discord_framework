use std::marker::PhantomData;

use serenity::all::{
    CommandData, CommandDataOptionValue, CommandOptionType, CreateCommand, CreateCommandOption,
    RoleId, UserId,
};

pub trait FromOptionValue: Sized {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self>;
    fn kind() -> CommandOptionType;
    fn reqired() -> bool {
        true
    }
    fn default() -> Option<Self> {
        None
    }
}

impl FromOptionValue for String {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self> {
        match value {
            CommandDataOptionValue::String(s) => Some(s.to_string()),
            _ => None,
        }
    }

    fn kind() -> CommandOptionType {
        CommandOptionType::String
    }
}

impl FromOptionValue for bool {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self> {
        match value {
            CommandDataOptionValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    fn kind() -> CommandOptionType {
        CommandOptionType::Boolean
    }
}

impl FromOptionValue for i64 {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self> {
        match value {
            CommandDataOptionValue::Integer(i) => Some(*i),
            _ => None,
        }
    }

    fn kind() -> CommandOptionType {
        CommandOptionType::Integer
    }
}

impl FromOptionValue for f64 {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self> {
        match value {
            CommandDataOptionValue::Number(f) => Some(*f),
            _ => None,
        }
    }

    fn kind() -> CommandOptionType {
        CommandOptionType::Number
    }
}

impl FromOptionValue for UserId {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self> {
        match value {
            CommandDataOptionValue::User(u) => Some(*u),
            _ => None,
        }
    }

    fn kind() -> CommandOptionType {
        CommandOptionType::User
    }
}

impl FromOptionValue for RoleId {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self> {
        match value {
            CommandDataOptionValue::Role(r) => Some(*r),
            _ => None,
        }
    }

    fn kind() -> CommandOptionType {
        CommandOptionType::Role
    }
}

impl<T: FromOptionValue> FromOptionValue for Option<T> {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self> {
        Some(T::from_option_value(value))
    }

    fn kind() -> CommandOptionType {
        T::kind()
    }

    fn reqired() -> bool {
        false
    }

    fn default() -> Option<Self> {
        Some(None)
    }
}

pub trait CommandDataExt {
    fn value<T: FromOptionValue>(&self, field: &str) -> Option<T>;
    fn arg<T: FromOptionValue>(&self, arg: &Arg<T>) -> Option<T>;
}

impl CommandDataExt for CommandData {
    fn value<T: FromOptionValue>(&self, field: &str) -> Option<T> {
        self.options
            .iter()
            .find(|opt| opt.name == field)
            .and_then(|opt| T::from_option_value(&opt.value))
            .or_else(T::default)
    }

    fn arg<T: FromOptionValue>(&self, arg: &Arg<T>) -> Option<T> {
        self.options
            .iter()
            .find(|opt| opt.name == arg.name)
            .and_then(|opt| T::from_option_value(&opt.value))
            .or_else(T::default)
    }
}

pub struct Arg<T> {
    pub name: &'static str,
    pub description: &'static str,
    pub autocomplete: bool,
    arg_t: PhantomData<T>,
}

impl<T: FromOptionValue> Arg<T> {
    fn value(&self, data: &CommandData) -> Option<T> {
        data.arg(self)
    }
}

impl<T: FromOptionValue> Arg<T> {
    fn as_option(&self) -> CreateCommandOption<'static> {
        CreateCommandOption::new(
            T::kind(),
            self.name.to_string(),
            self.description.to_string(),
        )
        .set_autocomplete(self.autocomplete)
        .required(T::reqired())
    }
}

impl<T> Arg<T> {
    pub const fn new(name: &'static str, description: &'static str, autocomplete: bool) -> Self {
        Arg {
            name,
            description,
            autocomplete,
            arg_t: PhantomData,
        }
    }
}

pub trait ArgList {
    type Output;
    fn add_options(&self, command: CreateCommand<'static>) -> CreateCommand<'static> {
        self.add_options_with(command, |_, o| o)
    }

    fn add_options_with(
        &self,
        command: CreateCommand<'static>,
        extra: fn(&str, CreateCommandOption<'static>) -> CreateCommandOption<'static>,
    ) -> CreateCommand<'static>;
    fn parse(&self, data: &CommandData) -> anyhow::Result<Self::Output>;
}

macro_rules! tuple_impls {
    ($T:ident) => {
        tuple_impls!(@impl $T);
    };
    ($T:ident $( $U:ident )+) => {
        tuple_impls!($( $U )+);
        tuple_impls!(@impl $T $( $U )+);
    };
    (@impl $( $T:ident )+) => {
        #[allow(nonstandard_style)]
        impl<$($T: FromOptionValue + std::fmt::Debug),+> ArgList for ($(Arg<$T>,)+) {
            type Output = ($( $T, )+);
            fn add_options_with(&self, command: CreateCommand<'static>, extra: fn(&str, CreateCommandOption<'static>)
                -> CreateCommandOption<'static>) -> CreateCommand<'static>
            {
                let ($($T,)+) = self;
                command $( .add_option(extra($T.name, $T.as_option())) )+
            }

            fn parse(&self, data: &CommandData) -> anyhow::Result<($( $T, )+)> {
                use anyhow::Context;
                let ($($T,)+) = self;
                Ok(($( $T.value(data).context(format!("no value for required argument {}", $T.name))?, )+))
            }
        }
    };
}

tuple_impls!(E D C B A Z Y X W V U T);

#[macro_export]
macro_rules! arg {
    ($arg:ident$([])*: $T:ty) => {
        $crate::Arg::<$T>::new(stringify!($arg), stringify!($arg), false)
    };
    ($arg:ident[autocomplete]: $T:ty) => {
        $crate::Arg::<$T>::new(stringify!($arg), stringify!($arg), true)
    };
    ($desc:literal $arg:ident$([])*: $T:ty) => {
        $crate::Arg::<$T>::new(stringify!($arg), $desc, false)
    };
    ($desc:literal $arg:ident[autocomplete]: $T:ty) => {
        $crate::Arg::<$T>::new(stringify!($arg), $desc, true)
    };
}

#[macro_export]
macro_rules! args {
    ($name:ident = $( $($desc:literal)* $arg:ident$([$($extra:ident)*])*: $T:ty ),* $(,)*) => {
        #[allow(nonstandard_style)]
        type $name = ( $( $T, )+ );
        const $name: ( $($crate::Arg<$T>,)+ ) = ( $( $crate::arg!($($desc)* $arg[$($($extra)*)*]: $T), )+ );
    };
}

#[macro_export]
macro_rules! args2 {
    (@parsed ($( $Ts:ident, )*) ($( $args:expr, )*) $name:ident =) => {
        const $name: ( $($crate::Arg<$Ts>,)* ) = ( $( $args, )*);
    };
    (@parsed ($( $Ts:ident, )*) ($( $args:expr, )*) $name:ident = $desc:literal $arg:ident: $T:ty $(,)*) => {
        const $name: ( $($crate::Arg<$Ts>,)* $crate::Arg<$T>, ) = ( $( $args, )* arg!($desc $arg: $T),);
    };
    (@parsed ($( $Ts:ident, )*) ($( $args:expr, )*) $name:ident = $desc:literal $arg:ident[autocomplete]: $T:ty $(,)*) => {
        const $name: ( $($crate::Arg<$Ts>,)* $crate::Arg<$T>, ) = ( $( $args, )* arg!($desc $arg[autocomplete]: $T),);
    };
    (@parsed ($( $Ts:ident, )*) ($( $args:expr, )*) $name:ident = $desc:literal $arg:ident: $T:ty, $($rem:tt)*) => {
        args2!(@parsed ($( $Ts, )* $T) ($( $args, )* arg!($desc $arg: T)) $name: $($rem)*)
    };
    (@parsed ($( $Ts:ident, )*) ($( $args:expr, )*) $name:ident = $desc:literal $arg:ident[autocomplete]: $T:ty, $($rem:tt)*) => {
        args2!(@parsed ($( $Ts, )* $T) ($( $args, )* arg!($desc $arg[autocomplete]: T)) $name: $($rem)*)
    };
    ($name:ident = $($rem:tt)*) => {args2!(@parsed () () $($rem)*)};
}

// trait Argument<T: FromOptionValue, P> {
//     const NAME: &'static str;
//     type WITH<U, const N: &'static str> = Argument<U, Self>;
// }
