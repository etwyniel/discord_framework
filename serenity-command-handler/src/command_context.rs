use anyhow::anyhow;
use serenity::{
    all::{
        Component, CreateAttachment, GenericChannelId, GuildId, Label, LabelComponent, Member,
        ModalInteraction, RoleId, User,
    },
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
pub trait InteractionExt {
    fn channel_id(&self) -> GenericChannelId;
    fn guild_id(&self) -> anyhow::Result<GuildId>;

    fn user(&self) -> &User;

    fn member(&self) -> Option<&Member>;

    async fn create_response(
        &self,
        http: &Http,
        builder: CreateInteractionResponse<'_>,
    ) -> serenity::Result<()>;

    async fn get_response(&self, http: &Http) -> serenity::Result<Message>;
}

#[async_trait]
impl InteractionExt for &CommandInteraction {
    fn channel_id(&self) -> GenericChannelId {
        self.channel_id
    }

    fn guild_id(&self) -> anyhow::Result<GuildId> {
        self.guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))
    }

    fn user(&self) -> &User {
        &self.user
    }

    fn member(&self) -> Option<&Member> {
        self.member.as_deref()
    }

    async fn create_response(
        &self,
        http: &Http,
        builder: CreateInteractionResponse<'_>,
    ) -> serenity::Result<()> {
        CommandInteraction::create_response(self, http, builder).await
    }

    async fn get_response(&self, http: &Http) -> serenity::Result<Message> {
        CommandInteraction::get_response(self, http).await
    }
}

#[async_trait]
impl InteractionExt for ModalInteraction {
    fn channel_id(&self) -> GenericChannelId {
        self.channel_id
    }

    fn guild_id(&self) -> anyhow::Result<GuildId> {
        self.guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))
    }

    fn user(&self) -> &User {
        &self.user
    }

    fn member(&self) -> Option<&Member> {
        self.member.as_ref()
    }

    async fn create_response(
        &self,
        http: &Http,
        builder: CreateInteractionResponse<'_>,
    ) -> serenity::Result<()> {
        ModalInteraction::create_response(self, http, builder).await
    }

    async fn get_response(&self, http: &Http) -> serenity::Result<Message> {
        ModalInteraction::get_response(self, http).await
    }
}

#[async_trait]
impl<T: InteractionExt + Sync> InteractionExt for &T {
    fn channel_id(&self) -> GenericChannelId {
        (*self).channel_id()
    }

    fn guild_id(&self) -> anyhow::Result<GuildId> {
        (*self).guild_id()
    }

    fn user(&self) -> &User {
        (*self).user()
    }

    fn member(&self) -> Option<&Member> {
        (*self).member()
    }

    async fn create_response(
        &self,
        http: &Http,
        builder: CreateInteractionResponse<'_>,
    ) -> serenity::Result<()> {
        (*self).create_response(http, builder).await
    }

    async fn get_response(&self, http: &Http) -> serenity::Result<Message> {
        (*self).get_response(http).await
    }
}

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
impl<T: InteractionExt + Sync> Responder for T {
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
            let roles = if let Some(r) = role_id {
                vec![RoleId::new(r)]
            } else {
                Vec::new()
            };
            msg = msg
                .content(&contents)
                .flags(flags)
                .allowed_mentions(CreateAllowedMentions::new().roles(roles));
            for (url, filename) in attachments.into_iter().flatten() {
                msg = msg.add_file(CreateAttachment::url(http, url, filename).await?);
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

pub fn get_text_input_value<'a>(components: &'a [Component], id: &'_ str) -> Option<&'a str> {
    components.iter().find_map(|row| {
        let Component::Label(Label {
            component: LabelComponent::InputText(t),
            ..
        }) = row
        else {
            return None;
        };
        if t.custom_id == id {
            t.value.as_deref()
        } else {
            None
        }
    })
}

pub fn get_select_values<'a>(components: &'a [Component], id: &'_ str) -> &'a [String] {
    components
        .iter()
        .find_map(|row| {
            let Component::Label(Label {
                component: LabelComponent::SelectMenu(m),
                ..
            }) = row
            else {
                return None;
            };
            if m.custom_id == id {
                Some(m.values.as_slice())
            } else {
                None
            }
        })
        .unwrap_or(&[])
}
