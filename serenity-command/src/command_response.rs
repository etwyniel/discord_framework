use serenity::{all::MessageFlags, builder::CreateEmbed};

#[derive(Debug)]
pub enum ResponseType {
    Text(String),
    Embed(Box<CreateEmbed<'static>>),
    Mixed(String, Vec<CreateEmbed<'static>>),
    WithAttachments(String, Vec<CreateEmbed<'static>>, Vec<(String, String)>),
}

impl From<String> for ResponseType {
    fn from(value: String) -> Self {
        ResponseType::Text(value)
    }
}

impl<'a> From<&'a str> for ResponseType {
    fn from(value: &'a str) -> Self {
        ResponseType::Text(value.to_string())
    }
}

impl From<CreateEmbed<'static>> for ResponseType {
    fn from(value: CreateEmbed<'static>) -> Self {
        ResponseType::Embed(Box::new(value))
    }
}

impl From<Box<CreateEmbed<'static>>> for ResponseType {
    fn from(value: Box<CreateEmbed<'static>>) -> Self {
        ResponseType::Embed(value)
    }
}

impl<T: Into<String>> From<(T, Vec<CreateEmbed<'static>>)> for ResponseType {
    fn from((text, embeds): (T, Vec<CreateEmbed<'static>>)) -> Self {
        ResponseType::Mixed(text.into(), embeds)
    }
}

#[derive(Debug)]
pub enum CommandResponse {
    None,
    Ack,
    Public(ResponseType),
    Private(ResponseType),
}

impl ResponseType {
    pub fn to_content(
        self,
    ) -> (
        Option<String>,
        Option<Vec<CreateEmbed<'static>>>,
        Option<Vec<(String, String)>>,
    ) {
        match self {
            ResponseType::Text(s) => (Some(s), None, None),
            ResponseType::Embed(e) => (None, Some(vec![*e]), None),
            ResponseType::Mixed(s, e) => (Some(s), Some(e), None),
            ResponseType::WithAttachments(s, e, a) => (Some(s), Some(e), Some(a)),
        }
    }
}

pub struct ContentAndFlags(
    pub String,
    pub Option<Vec<CreateEmbed<'static>>>,
    pub Option<Vec<(String, String)>>,
    pub MessageFlags,
);

impl CommandResponse {
    pub const ACK: anyhow::Result<Self> = Ok(CommandResponse::Ack);

    pub fn to_contents_and_flags(self) -> Option<ContentAndFlags> {
        Some(match self {
            CommandResponse::None | CommandResponse::Ack => return None,
            CommandResponse::Public(resp) => {
                let (text, embeds, attachments) = resp.to_content();
                ContentAndFlags(
                    text.unwrap_or_default(),
                    embeds,
                    attachments,
                    MessageFlags::empty(),
                )
            }
            CommandResponse::Private(resp) => {
                let (text, embeds, attachments) = resp.to_content();
                ContentAndFlags(
                    text.unwrap_or_default(),
                    embeds,
                    attachments,
                    MessageFlags::EPHEMERAL,
                )
            }
        })
    }

    pub fn public<T: Into<ResponseType>>(value: T) -> anyhow::Result<Self> {
        Ok(Self::Public(value.into()))
    }

    pub fn private<T: Into<ResponseType>>(value: T) -> anyhow::Result<Self> {
        Ok(Self::Private(value.into()))
    }
}

impl<T: Into<ResponseType>> From<T> for CommandResponse {
    fn from(value: T) -> Self {
        CommandResponse::Public(value.into())
    }
}
