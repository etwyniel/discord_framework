use std::{borrow::Cow, sync::Arc};

use anyhow::{Context as _, bail};
use chrono::Timelike as _;
use reqwest::Url;
use serenity::all::{
    CommandInteraction, ComponentInteraction, Context, CreateActionRow, CreateButton,
    CreateComponent, CreateInputText, CreateInteractionResponse, CreateInteractionResponseFollowup,
    CreateInteractionResponseMessage, CreateLabel, CreateModal, CreateModalComponent,
    CreateSelectMenu, CreateSelectMenuKind, EditInteractionResponse, GenericChannelId, GuildId,
    Http, InputTextStyle, InteractionId, Member, MessageCommandInteractionMetadata,
    MessageInteractionMetadata, ModalInteraction, Permissions, RoleId,
};
use serenity_command::args;
use tokio::sync::mpsc::Receiver;

use crate::prelude::*;
use crate::{album::Album, command_context::InteractionInfo};

use super::{Lp, LpCreationEvent, ModLp, ResolvedLp};

struct LpCreationModalHandler {
    handler: Arc<Handler>,
    http: Arc<Http>,
    receiver: Receiver<LpCreationEvent>,
    interaction_handle: (InteractionId, String),
    member: Member,
    guild_id: GuildId,
    channel_id: GenericChannelId,
}

impl LpCreationModalHandler {
    fn new(
        handler: &Handler,
        command: &CommandInteraction,
        receiver: Receiver<LpCreationEvent>,
    ) -> anyhow::Result<Self> {
        let member = command
            .member
            .as_deref()
            .cloned()
            .context("expected member")?;
        Ok(LpCreationModalHandler {
            handler: handler.me.upgrade().unwrap(),
            http: Arc::clone(handler.http.get().unwrap()),
            receiver,
            interaction_handle: (command.id, command.token.to_string()),
            member,
            guild_id: command.guild_id()?,
            channel_id: command.channel_id,
        })
    }

    async fn wait_for_submit(mut self) {
        // await initial configuration from the modal submission
        let (modal_id, modal_token, album, link, description, time) = loop {
            match self.receiver.recv().await {
                None => return,
                Some(LpCreationEvent::Initial {
                    modal_id,
                    modal_token,
                    album,
                    link,
                    description,
                    time,
                }) => break (modal_id, modal_token, album, link, description, time),
                _ => continue, // should be unreachable but handle to avoid breakage
            }
        };

        // build params, resolve starting time, album info, role, genres...
        let lp = Lp {
            album,
            link,
            time,
            provider: None,
            role: None,
        };
        let Ok((resolved, info)) = lp.resolve(&self.handler, self.guild_id).await else {
            return;
        };

        // build initial message contents
        let contents = match resolved.build_message_contents(
            &info,
            resolved.params.role.map(RoleId::get),
            description.as_deref(),
        ) {
            Ok(contents) => contents,
            Err(e) => {
                let err_msg = format!("Failed to create listening party: {e:?}");
                eprintln!("{err_msg}");
                // attempt to return error to user, ignore failure
                _ = CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(err_msg)
                        .ephemeral(true),
                )
                .execute(&self.http, modal_id, &modal_token)
                .await;
                return;
            }
        };

        // build components to add to the LP creation message
        let message_components =
            listening_party_creator_components(Some(&self.member), resolved.params.role);
        // create ephemeral editable message
        let res = CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .ephemeral(true)
                .content(format!("# PREVIEW\n{contents}"))
                .components(message_components),
        )
        .execute(&self.http, modal_id, &modal_token)
        .await;
        if let Err(e) = res {
            eprintln!("failed to create ephemeral LP creation message: {e:?}");
            return;
        }

        // start event routine
        let (id, token) = self.interaction_handle;
        let mut creator = LpCreator {
            id,
            token,
            channel_id: self.channel_id,
            guild_id: self.guild_id,
            member: self.member,
            modal_token,
            lp: resolved,
            info,
            desc: description,
        };
        creator.run(&self.handler, &self.http, self.receiver).await;
    }
}

fn lp_creation_modal(command_id: InteractionId) -> CreateInteractionResponse<'static> {
    // create modal fields
    let album_field = CreateLabel::input_text(
        "Album",
        CreateInputText::new(InputTextStyle::Short, "album").required(true),
    )
    .description("Album link, listening party title, or album search query");
    let link_field = CreateLabel::input_text(
        "Link",
        CreateInputText::new(InputTextStyle::Short, "link").required(false),
    )
    .description("Optional");
    let time_field = CreateLabel::input_text(
        "Time",
        CreateInputText::new(InputTextStyle::Short, "time")
            .required(false)
            .placeholder("+5"),
    )
    .description("Listening Party time (e.g. +5, XX:20)");
    let description_field = CreateLabel::input_text(
        "Description",
        CreateInputText::new(InputTextStyle::Paragraph, "description").required(false),
    );
    let fields = vec![
        CreateModalComponent::Label(album_field),
        CreateModalComponent::Label(link_field),
        CreateModalComponent::Label(description_field),
        CreateModalComponent::Label(time_field),
    ];

    CreateInteractionResponse::Modal(
        CreateModal::new(
            format!("create_lp.{}", command_id),
            "Start a Listening Party",
        )
        .components(fields),
    )
}

pub const CREATE_LP: CommandConst = CommandConst {
    description: "Create a Listening Party",
    ..command!(/create_listening_party: create_lp)
};

async fn create_lp(
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    // create channel for events, store sender in module
    let mod_lp: Arc<ModLp> = handler.module_arc()?;
    let receiver = {
        let (sender, receiver) = tokio::sync::mpsc::channel::<LpCreationEvent>(5);
        mod_lp
            .lp_creation_events
            .write()
            .await
            .insert(command.id, sender);
        receiver
    };

    // create modal
    let modal = lp_creation_modal(command.id);
    command.create_response(&ctx.http, modal).await?;

    // get handles to values required for the event listening routine
    let modal_handler = LpCreationModalHandler::new(handler, command, receiver)?;
    let interaction_id = command.id;

    // spawn event handling routine, with a timeout
    tokio::spawn(async move {
        _ = tokio::time::timeout(
            std::time::Duration::from_mins(10),
            modal_handler.wait_for_submit(),
        )
        .await;
        // remove sender from module
        mod_lp
            .lp_creation_events
            .write()
            .await
            .remove(&interaction_id);
    });
    Ok(CommandResponse::None)
}

fn listening_party_creator_components(
    member: Option<&Member>,
    role_id: Option<RoleId>,
) -> Vec<CreateComponent<'static>> {
    let mut message_components = vec![];

    if let Some(member) = member
        && member
            .permissions
            .unwrap_or_default()
            .contains(Permissions::MENTION_EVERYONE)
    {
        let role_field = CreateSelectMenu::new(
            "select_listening_party_role",
            CreateSelectMenuKind::Role {
                default_roles: role_id.map(|role| Cow::Owned(vec![role])),
            },
        )
        .max_values(1);
        message_components.push(CreateComponent::ActionRow(CreateActionRow::SelectMenu(
            role_field,
        )));
    }
    message_components.push(CreateComponent::ActionRow(CreateActionRow::buttons(vec![
        CreateButton::new("button_edit_lp").emoji('✏').label("Edit"),
        CreateButton::new("button_send_lp").emoji('▶').label("Send"),
    ])));
    message_components
}

struct LpCreator {
    id: InteractionId,
    token: String,
    channel_id: GenericChannelId,
    modal_token: String,
    guild_id: GuildId,
    lp: ResolvedLp,
    info: Album,
    desc: Option<String>,
    member: Member,
}

impl LpCreator {
    fn build_contents(&mut self) -> anyhow::Result<String> {
        self.lp.build_message_contents(
            &self.info,
            self.lp.params.role.map(RoleId::get),
            self.desc.as_deref(),
        )
    }

    async fn edit_preview(&mut self, http: &Http) {
        let contents = self.build_contents().unwrap();
        let resp = EditInteractionResponse::new()
            .content(format!("# PREVIEW\n{contents}"))
            .execute(http, &self.modal_token)
            .await;
        if let Err(e) = resp {
            eprintln!("failed to edit ephemeral LP creation message: {e:?}");
        }
    }

    async fn send(&mut self, handler: &Handler, http: &Http) -> bool {
        let contents = self.build_contents().unwrap();
        let interaction = InteractionInfo {
            id: self.id,
            token: &self.token,
            guild_id: self.guild_id,
            channel_id: self.channel_id,
            member: &self.member,
        };
        // send listening party as specified by configuration in DB
        let res = Lp::send(handler, &self.lp, &interaction, &self.info, &contents, true).await;
        if let Err(e) = res {
            eprintln!("failed to send LP: {e:?}");
            // attempt to send a new ephemeral response to notify user
            _ = CreateInteractionResponseFollowup::new()
                .content(format!("failed to send listening party: {e:?}"))
                .ephemeral(true)
                .execute(http, None, &self.modal_token)
                .await;
            _ = EditInteractionResponse::new()
                .content(format!("failed to send listening party: {e:?}"))
                .execute(http, &self.modal_token)
                .await;
            return false;
        }
        _ = http
            .delete_original_interaction_response(&self.modal_token)
            .await;
        true
    }

    async fn run(
        &mut self,
        handler: &Handler,
        http: &Http,
        mut receiver: Receiver<LpCreationEvent>,
    ) {
        use super::LpCreationEvent::*;
        while let Some(evt) = receiver.recv().await {
            match evt {
                Initial { .. } => continue, // should not happen, ignore and carry on
                Send => {
                    // done editing, send final version
                    if self.send(handler, http).await {
                        // send succeeded, done
                        return;
                    }
                    // send failed, stay in loop to allow retrying
                }
                ChangeRole(new_role) => {
                    self.lp.params.role = Some(new_role);
                    self.edit_preview(http).await;
                }
                Edit {
                    album: title,
                    link,
                    description,
                    time,
                } => {
                    let params = &mut self.lp.params;
                    if params.album != title || params.link != link {
                        // changed which album is being listened to, resolve album info again
                        let lp = Lp {
                            album: title,
                            link,
                            time,
                            role: params.role,
                            provider: params.provider.clone(),
                        };
                        let Ok((resolved, info)) = lp.resolve(handler, self.guild_id).await else {
                            continue;
                        };
                        self.lp = resolved;
                        self.info = info;
                    } else {
                        if params.time != time {
                            self.lp.resolved_start = None;
                        }
                        params.time = time;
                    }
                    self.desc = description;
                    self.edit_preview(http).await;
                }
            }
        }
    }
}

args!(SUBMIT_CREATE_LP_ARGS =
    album: String,
    link: Option<String>,
    description: Option<String>,
    time: Option<String>,
);

pub const SUBMIT_CREATE_LP: ModalCommandConst =
    modal_command!(create_lp SUBMIT_CREATE_LP_ARGS: submit_create_lp);

fn get_interaction_id(custom_id: &str) -> anyhow::Result<InteractionId> {
    custom_id
        .split('.')
        .nth(1)
        .and_then(|s| s.parse::<u64>().ok())
        .map(InteractionId::new)
        .context("Cannot find interaction ID")
}

// handle submit of LP creation modal
async fn submit_create_lp(
    (album, link, description, time): SUBMIT_CREATE_LP_ARGS,
    handler: &Handler,
    _ctx: &Context,
    modal: &ModalInteraction,
) -> anyhow::Result<CommandResponse> {
    // retrieve ID of the original command interaction
    let original_interaction_id = get_interaction_id(&modal.data.custom_id)?;
    let mod_lp: &ModLp = handler.module()?;

    let evt = LpCreationEvent::Initial {
        modal_id: modal.id,
        modal_token: modal.token.to_string(),
        album,
        link,
        description,
        time,
    };
    mod_lp.dispatch_event(original_interaction_id, evt).await?;
    Ok(CommandResponse::None)
}

pub const BUTTON_SEND_LP: ComponentCommandConst =
    component_command!(button_send_lp: button_send_lp);

// handle start button for LP creator
async fn button_send_lp(
    handler: &Handler,
    _ctx: &Context,
    component: &ComponentInteraction,
) -> anyhow::Result<CommandResponse> {
    // retrieve ID of the original command interaction
    let msg = &component.message;
    let Some(MessageInteractionMetadata::ModalSubmit(modal_data)) =
        msg.interaction_metadata.as_deref()
    else {
        return CommandResponse::ACK;
    };
    let MessageInteractionMetadata::Command(MessageCommandInteractionMetadata {
        id: interaction_id,
        ..
    }) = modal_data.triggering_interaction_metadata.as_ref()
    else {
        return CommandResponse::ACK;
    };

    // use interaction ID to find sender to LP creator routine
    let mod_lp: &ModLp = handler.module()?;
    mod_lp
        .dispatch_event(*interaction_id, LpCreationEvent::Send)
        .await?;
    CommandResponse::ACK
}

args!(SELECT_LP_ROLE_ARGS = role: RoleId);

pub const CHANGE_LP_ROLE: ComponentCommandConst =
    component_command!(select_listening_party_role SELECT_LP_ROLE_ARGS: select_lp_role);

// handle role selection message component for LP creation
async fn select_lp_role(
    (role,): SELECT_LP_ROLE_ARGS,
    handler: &Handler,
    _ctx: &Context,
    component: &ComponentInteraction,
) -> anyhow::Result<CommandResponse> {
    // retrieve ID of the original command interaction
    let msg = &component.message;
    let Some(MessageInteractionMetadata::ModalSubmit(modal_data)) =
        msg.interaction_metadata.as_deref()
    else {
        return CommandResponse::ACK;
    };
    let MessageInteractionMetadata::Command(MessageCommandInteractionMetadata {
        id: interaction_id,
        ..
    }) = modal_data.triggering_interaction_metadata.as_ref()
    else {
        return CommandResponse::ACK;
    };

    // use interaction ID to find sender to LP creator routine
    let mod_lp: &ModLp = handler.module()?;
    if let Some(sender) = mod_lp.lp_creation_events.read().await.get(interaction_id) {
        sender.send(LpCreationEvent::ChangeRole(role)).await?;
    } else {
        bail!("Interaction not found");
    }
    CommandResponse::ACK
}

pub const BUTTON_EDIT_LP: ComponentCommandConst =
    component_command!(button_edit_lp: button_edit_lp);

async fn button_edit_lp(
    _handler: &Handler,
    ctx: &Context,
    component: &ComponentInteraction,
) -> anyhow::Result<CommandResponse> {
    // retrieve ID of the original command interaction
    let msg = &component.message;
    let Some(MessageInteractionMetadata::ModalSubmit(modal_data)) =
        msg.interaction_metadata.as_deref()
    else {
        return CommandResponse::ACK;
    };
    let MessageInteractionMetadata::Command(MessageCommandInteractionMetadata {
        id: interaction_id,
        ..
    }) = modal_data.triggering_interaction_metadata.as_ref()
    else {
        return CommandResponse::ACK;
    };

    // extract information about current LP configuration from message
    let Some(pos) = msg.content.find(super::LP_URI) else {
        return CommandResponse::private("no embedded data");
    };
    let url: Url = msg.content[pos..]
        .split_once(')')
        .and_then(|(url, _)| url.parse().ok())
        .context("invalid embedded URL")?;
    let lp: ResolvedLp = serde_urlencoded::de::from_str(url.query().unwrap_or_default())
        .context("failed to deserialize embedded data")?;

    // create modal fields, pre-filled with current values
    let mut album_input = CreateInputText::new(InputTextStyle::Short, "album").required(true);
    if let Some(title) = &lp.resolved_title {
        album_input = album_input.value(title);
    }
    let album_field = CreateLabel::input_text("Album", album_input)
        .description("Album link, listening party title, or album search query");

    let mut link_input = CreateInputText::new(InputTextStyle::Short, "link").required(false);
    if let Some(link) = &lp.resolved_link {
        link_input = link_input.value(link);
    }
    let link_field = CreateLabel::input_text("Link", link_input).description("Optional");

    let mut time_input = CreateInputText::new(InputTextStyle::Short, "time")
        .required(false)
        .placeholder("+5");
    if let Some(time) = &lp.resolved_start {
        let minute = time.minute();
        time_input = time_input.value(format!("XX:{minute:02}"));
    }
    let time_field = CreateLabel::input_text("Time", time_input)
        .description("Listening Party time (e.g. +5, XX:20)");

    // extract description if any
    let desc = msg.content.split(super::SEPARATOR).nth(3);
    let mut description_input =
        CreateInputText::new(InputTextStyle::Paragraph, "description").required(false);
    if let Some(desc) = desc {
        description_input = description_input.value(desc);
    }
    let description_field = CreateLabel::input_text("Description", description_input);
    let fields = vec![
        CreateModalComponent::Label(album_field),
        CreateModalComponent::Label(link_field),
        CreateModalComponent::Label(description_field),
        CreateModalComponent::Label(time_field),
    ];
    component
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Modal(
                CreateModal::new(
                    format!("edit_lp_creator.{interaction_id}"),
                    "Edit Listening Party",
                )
                .components(fields),
            ),
        )
        .await?;
    Ok(CommandResponse::None)
}

// use the same arguments as the create LP modal
pub const SUBMIT_EDIT_LP: ModalCommandConst =
    modal_command!(edit_lp_creator SUBMIT_CREATE_LP_ARGS: submit_edit_lp);

// handle submit from edition modal for LP creator
async fn submit_edit_lp(
    (album, link, description, time): SUBMIT_CREATE_LP_ARGS,
    handler: &Handler,
    _ctx: &Context,
    modal: &ModalInteraction,
) -> anyhow::Result<CommandResponse> {
    // extract original interaction ID, stored in modal custom_id
    let original_interaction_id = get_interaction_id(&modal.data.custom_id)?;

    // use interaction ID to find sender to LP creator routine
    let evt = LpCreationEvent::Edit {
        album,
        link,
        description,
        time,
    };
    let mod_lp: &ModLp = handler.module()?;
    mod_lp.dispatch_event(original_interaction_id, evt).await?;
    CommandResponse::ACK
}
