use std::marker::PhantomData;

use serenity::all::{
    CommandData, CommandDataOptionValue, CommandOptionType, ComponentInteractionData,
    ComponentInteractionDataKind, CreateCommand, CreateCommandOption, LabelComponent,
    ModalComponent, ModalInteractionData, RoleId, UserId,
};

pub trait FromOptionValue: Sized {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self>;
    fn from_component(_value: &ComponentInteractionDataKind) -> Option<Self> {
        None
    }
    fn from_str(_value: &str) -> Option<Self> {
        None
    }
    fn from_strings(_value: &[String]) -> Option<Self> {
        None
    }
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

    fn from_component(value: &ComponentInteractionDataKind) -> Option<Self> {
        let ComponentInteractionDataKind::StringSelect { values } = value else {
            return None;
        };
        values.first().map(|s| s.to_string())
    }

    fn from_str(value: &str) -> Option<Self> {
        if value.is_empty() {
            return None;
        }
        Some(value.to_string())
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

    fn from_component(value: &ComponentInteractionDataKind) -> Option<Self> {
        let ComponentInteractionDataKind::UserSelect { values } = value else {
            return None;
        };
        values.first().copied()
    }

    fn from_str(value: &str) -> Option<Self> {
        Some(UserId::new(value.parse::<u64>().ok()?))
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

    fn from_component(value: &ComponentInteractionDataKind) -> Option<Self> {
        let ComponentInteractionDataKind::RoleSelect { values } = value else {
            return None;
        };
        values.first().copied()
    }

    fn from_str(value: &str) -> Option<Self> {
        Some(RoleId::new(value.parse::<u64>().ok()?))
    }

    fn kind() -> CommandOptionType {
        CommandOptionType::Role
    }
}

impl<T: FromOptionValue> FromOptionValue for Option<T> {
    fn from_option_value(value: &CommandDataOptionValue) -> Option<Self> {
        Some(T::from_option_value(value))
    }

    fn from_component(value: &ComponentInteractionDataKind) -> Option<Self> {
        Some(T::from_component(value))
    }

    fn from_str(value: &str) -> Option<Self> {
        Some(T::from_str(value))
    }

    fn from_strings(value: &[String]) -> Option<Self> {
        Some(T::from_strings(value))
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

impl<T: FromOptionValue> FromOptionValue for Vec<T> {
    fn from_option_value(_: &CommandDataOptionValue) -> Option<Self> {
        None
    }

    // FIXME handle from_component

    fn kind() -> CommandOptionType {
        panic!("Array arguments can only be used in modals and components")
    }

    fn from_strings(value: &[String]) -> Option<Self> {
        value.iter().map(|v| T::from_str(v)).collect()
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

impl CommandDataExt for ModalInteractionData {
    fn value<T: FromOptionValue>(&self, field: &str) -> Option<T> {
        let components = self.components.as_slice();
        for comp in components {
            let ModalComponent::Label(label) = comp else {
                continue;
            };
            match &label.component {
                LabelComponent::InputText(text) if text.custom_id == field => {
                    return T::from_str(text.value.as_deref()?);
                }
                LabelComponent::SelectMenu(select) if select.custom_id == field => {
                    if select.values.len() == 1 {
                        let single = T::from_str(select.values[0].as_str());
                        if single.is_some() {
                            return single;
                        }
                    }
                    return T::from_strings(select.values.as_slice());
                }
                _ => continue,
            }
        }
        T::default()
    }

    fn arg<T: FromOptionValue>(&self, arg: &Arg<T>) -> Option<T> {
        self.value(arg.name)
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
    fn parse_modal(&self, data: &ModalInteractionData) -> anyhow::Result<Self::Output>;
    fn parse_component(&self, data: &ComponentInteractionData) -> anyhow::Result<Self::Output>;
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

            fn parse_modal(&self, data: &ModalInteractionData) -> anyhow::Result<($( $T, )+)> {
                use anyhow::Context;
                use $crate::CommandDataExt;
                let ($($T,)+) = self;
                Ok(($( data.arg($T).context(format!("no appropriate value found for argument {}", $T.name))?, )+))
            }

            fn parse_component(&self, data: &ComponentInteractionData) -> anyhow::Result<($( $T, )+)> {
                use anyhow::Context;
                let ($($T,)+) = self;
                Ok(($( $T::from_component(&data.kind).context(format!("no appropriate value found for argument {}", $T.name))?, )+))
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
