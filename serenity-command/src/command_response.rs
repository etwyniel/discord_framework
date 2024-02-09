use serenity::{all::InteractionResponseFlags, builder::CreateEmbed};

#[derive(Debug)]
pub enum ResponseType {
    Text(String),
    Embed(Box<CreateEmbed>),
    Mixed(String, Vec<CreateEmbed>),
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

impl From<CreateEmbed> for ResponseType {
    fn from(value: CreateEmbed) -> Self {
        ResponseType::Embed(Box::new(value))
    }
}

impl From<Box<CreateEmbed>> for ResponseType {
    fn from(value: Box<CreateEmbed>) -> Self {
        ResponseType::Embed(value)
    }
}

impl<T: Into<String>> From<(T, Vec<CreateEmbed>)> for ResponseType {
    fn from((text, embeds): (T, Vec<CreateEmbed>)) -> Self {
        ResponseType::Mixed(text.into(), embeds)
    }
}

#[derive(Debug)]
pub enum CommandResponse {
    None,
    Public(ResponseType),
    Private(ResponseType),
}

impl ResponseType {
    pub fn to_content(self) -> (Option<String>, Option<Vec<CreateEmbed>>) {
        match self {
            ResponseType::Text(s) => (Some(s), None),
            ResponseType::Embed(e) => (None, Some(vec![*e])),
            ResponseType::Mixed(s, e) => (Some(s), Some(e)),
        }
    }
}

impl CommandResponse {
    pub fn to_contents_and_flags(
        self,
    ) -> Option<(String, Option<Vec<CreateEmbed>>, InteractionResponseFlags)> {
        Some(match self {
            CommandResponse::None => return None,
            CommandResponse::Public(resp) => {
                let (text, embeds) = resp.to_content();
                (
                    text.unwrap_or_default(),
                    embeds,
                    InteractionResponseFlags::empty(),
                )
            }
            CommandResponse::Private(resp) => {
                let (text, embeds) = resp.to_content();
                (
                    text.unwrap_or_default(),
                    embeds,
                    InteractionResponseFlags::EPHEMERAL,
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
