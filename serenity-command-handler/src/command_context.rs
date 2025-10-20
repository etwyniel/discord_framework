use serenity::{
    all::CreateAttachment,
    async_trait,
    builder::{CreateAllowedMentions, CreateInteractionResponse, CreateInteractionResponseMessage},
    http::Http,
    model::{
        application::{CommandDataOption, CommandDataOptionValue, CommandInteraction},
        channel::Message,
    },
};

use serenity_command::{CommandResponse, ContentAndFlags};

#[async_trait]
pub trait Responder {
    async fn respond(
        &self,
        http: &Http,
        contents: CommandResponse,
        role_id: Option<u64>,
    ) -> anyhow::Result<Option<Message>>;
}

#[async_trait]
impl Responder for CommandInteraction {
    async fn respond(
        &self,
        http: &Http,
        contents: CommandResponse,
        role_id: Option<u64>,
    ) -> anyhow::Result<Option<Message>> {
        let ContentAndFlags(contents, embeds, attachments, flags) =
            match contents.to_contents_and_flags() {
                None => return Ok(None),
                Some(c) => c,
            };
        self.create_response(http, {
            let mut msg = CreateInteractionResponseMessage::new();
            msg = embeds
                .into_iter()
                .flatten()
                .fold(msg, |msg, embed| msg.add_embed(embed));
            msg = msg
                .content(&contents)
                .flags(flags)
                .allowed_mentions(CreateAllowedMentions::new().roles(role_id));
            for att in attachments.iter().flatten() {
                msg = msg.add_file(CreateAttachment::url(http, att).await?);
            }
            CreateInteractionResponse::Message(msg)
        })
        .await?;
        self.get_response(http)
            .await
            .map_err(anyhow::Error::from)
            .map(Some)
    }
}

pub fn get_str_opt_ac<'a>(options: &'a [CommandDataOption], name: &str) -> Option<&'a str> {
    options
        .iter()
        .find(|opt| opt.name == name)
        .and_then(|val| val.value.as_str())
}

#[allow(unused)]
pub fn get_int_opt_ac(options: &[CommandDataOption], name: &str) -> Option<i64> {
    options.iter().find(|opt| opt.name == name)?.value.as_i64()
}

pub fn get_focused_option(options: &[CommandDataOption]) -> Option<&str> {
    options
        .iter()
        .find(|opt| matches!(&opt.value, CommandDataOptionValue::Autocomplete { .. }))
        .map(|opt| opt.name.as_str())
}
