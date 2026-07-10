use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail};
use chrono::Duration;
use fallible_iterator::FallibleIterator;
use futures::FutureExt;
use itertools::Itertools;
use regex::Regex;
use rspotify::prelude::Id;
use rusqlite::{Connection, params};
use serenity::{
    async_trait,
    builder::{CreateCommand, CreateCommandOption, CreateEmbed},
    futures::future::BoxFuture,
    model::{
        Permissions,
        application::{CommandDataOptionValue, CommandInteraction, CommandOptionType},
        prelude::GuildId,
        user::User,
    },
    prelude::{Context, RwLock},
};
use tokio::task::block_in_place;

use crate::{
    RegisterableModule,
    db::Db,
    modules::{
        AlbumLookup, Spotify,
        google_apis::{self, Authenticator, sheets::Sheets},
    },
    prelude::*,
};
use serenity_command::{CommandKey, CommandResponse, args, command};

mod complete;
use complete::process_autocomplete;

pub mod model;
use model::*;

/// Default form response range
const DEFAULT_RANGE: &str = "B:Z";

const SCOPE_FORMS_READONLY: &str = "https://www.googleapis.com/auth/forms.body.readonly";

/// Converts `s` to a string that can be used as a command or option name
pub fn sanitize_name(s: &str) -> String {
    let temp = s.chars().filter(|c| c.is_ascii()).collect::<String>();
    let it = temp
        .trim()
        .chars()
        .map(|c| {
            if c.is_whitespace() || "-+&./".contains(c) {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .filter(|&c| c.is_alphanumeric() || c == '_');
    // initialize output, will not be larger than input string
    let mut out = String::with_capacity(s.len());
    let mut prev_was_underscore = false;
    for c in it {
        if out.len() >= 32 {
            // trim to 32 characters
            break;
        }
        // deduplicate underscores
        if c == '_' {
            if !prev_was_underscore {
                prev_was_underscore = true;
                out.push(c)
            }
            continue;
        }
        prev_was_underscore = false;
        out.push(c);
    }
    out
}

impl SimpleForm {
    /// Create a discord slash command from a form
    pub fn to_command(&self, command_name: &str) -> CreateCommand<'_> {
        let mut cmd = CreateCommand::new(sanitize_name(command_name)).description(&self.title);
        // skip first question, assumed to be username
        let mut questions = self.questions.iter().skip(1).collect::<Vec<_>>();
        // discord requires required options to be first
        questions.sort_by_key(|question| !question.required);

        // does the next option support completion
        let mut autocomplete = false;
        for (i, q) in questions.iter().enumerate() {
            let sanitized = sanitize_name(&q.title);
            if let Some(next) = questions.get(i + 1) {
                let next_lower = next.title.to_lowercase();
                if let QuestionType::Text = q.ty
                    && (next_lower.contains("spotify") || next_lower.contains("link"))
                {
                    // q is most likely asking for the song artist and name, which we will retrieve
                    // using the song url
                    autocomplete = true;
                    continue;
                }
            }
            let mut opt = CreateCommandOption::new(CommandOptionType::String, sanitized, &q.title)
                .required(q.required)
                .set_autocomplete(autocomplete);
            if let QuestionType::Choice(values) = &q.ty {
                // handle multiple choice questions
                opt = values
                    .iter()
                    .fold(opt, |opt, v| opt.add_string_choice(v, v));
            }
            cmd = cmd.add_option(opt);
            autocomplete = false;
        }
        cmd
    }
}

pub struct FormsClient {
    pub authenticator: Authenticator,
}

impl FormsClient {
    /// Fetch the definition of a form
    pub async fn get_form(&self, form_id: &str) -> anyhow::Result<SimpleForm> {
        let token = self.authenticator.get_token().await?;
        let url = format!("https://forms.googleapis.com/v1/forms/{form_id}");
        let form: Form = reqwest::Client::new()
            .get(url)
            .bearer_auth(token)
            .send()
            .await?
            .json()
            .await?;
        form.try_into()
    }
}

pub struct FormCommand {
    pub guild_id: u64,
    pub command_name: String,
    pub command_id: u64,
    pub form: SimpleForm,
    pub submission_type: String,
    pub submissions_range: Option<String>,
}

args!(COMMAND_FROM_FORM_ARGS =
    "The name of the command"
    command_name: String,
    "The edit id of the form to use (found in the url when editing it)"
    form_id: String,
    "Whether users will be submitting songs or albums"
    submission_type: Option<String>,
);

/// Configure options for the /command_from_form command
fn set_command_from_form_options(
    name: &str,
    opt: CreateCommandOption<'static>,
) -> CreateCommandOption<'static> {
    if name == "submission_type" {
        opt.add_string_choice("song", "song")
            .add_string_choice("album", "album")
    } else {
        opt
    }
}

pub const COMMAND_FROM_FORM: CommandConst = CommandConst {
    description: "Create a submission command from a Google Form",
    permissions: Permissions::MANAGE_EVENTS,
    ..command!(/command_from_form COMMAND_FROM_FORM_ARGS(set_command_from_form_options): command_from_form)
};

#[derive(Debug)]
pub struct CommandFromForm {
    pub command_name: String,
    pub form_id: String,
    pub submission_type: Option<String>,
}

/// Create a discord slash command from a google form, specified by its edit URL
async fn command_from_form(
    (command_name, form_id, submission_type): COMMAND_FROM_FORM_ARGS,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?;
    let params = CommandFromForm {
        command_name,
        form_id,
        submission_type,
    };
    params.add_form(handler, ctx, guild_id).await
}

impl CommandFromForm {
    /// Create a discord slash command from a google form, specified by its edit URL
    async fn add_form(
        mut self,
        handler: &Handler,
        ctx: &Context,
        guild_id: GuildId,
    ) -> anyhow::Result<CommandResponse> {
        // extract form ID from edit URL
        let spreadsheet_url_re = Regex::new(r#"https://docs.google.com/forms/d/([^/]+)"#).unwrap();
        if let Some(cap) = spreadsheet_url_re.captures(&self.form_id) {
            self.form_id = cap.get(1).unwrap().as_str().to_string();
        }
        let forms: &Forms = handler.module()?;
        let form = forms.forms_client.get_form(&self.form_id).await?;
        let cmd = form.to_command(&self.command_name);
        let cmd = guild_id.create_command(&ctx.http, cmd).await?;
        let resp = format!("Created command </{}:{}>", cmd.name, cmd.id.get());
        let form_json = serde_json::to_string(&form)?;
        let submission_type = self
            .submission_type
            .as_deref()
            .unwrap_or("song") // default submission type
            .to_string();

        // insert form definition into DB
        let db = handler.db.lock().await;
        block_in_place(|| {
            db.conn().execute(
                "INSERT INTO forms (guild_id, command_name, command_id, form, submission_type)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (guild_id, command_name) DO UPDATE
                 SET command_id = ?3, form = ?4, submission_type = ?5
                 WHERE guild_id = ?1 AND command_name = ?2",
                params![
                    guild_id.get(),
                    cmd.name.to_string(),
                    cmd.id.get(),
                    form_json,
                    &submission_type
                ],
            )
        })?;
        drop(db);

        // store form command in module to avoid having to access DB every time a command is executed
        let command = FormCommand {
            guild_id: guild_id.get(),
            command_name: cmd.name.to_string(),
            command_id: cmd.id.get(),
            form,
            submission_type,
            submissions_range: None,
        };
        let mut forms = forms.forms.write().await;
        if let Some(form) = forms
            .iter_mut()
            .find(|form| form.command_name == self.command_name)
        {
            *form = command;
        } else {
            forms.push(command);
        }
        CommandResponse::public(resp)
    }
}

pub async fn check_forms(handler: &Handler, ctx: &Context) -> anyhow::Result<()> {
    let mut to_re_add = Vec::new();
    {
        for form in handler.module::<Forms>()?.forms.read().await.iter() {
            if form.form.questions[0].id.is_empty() {
                to_re_add.push((
                    form.guild_id,
                    form.command_name.clone(),
                    form.form.id.clone(),
                    form.submission_type.clone(),
                ));
            }
        }
    }
    for (guild_id, command_name, form_id, submission_type) in to_re_add {
        CommandFromForm {
            form_id,
            command_name,
            submission_type: Some(submission_type),
        }
        .add_form(handler, ctx, GuildId::new(guild_id))
        .await?;
    }
    Ok(())
}

args!(REFRESH_FORM_COMMAND_ARGS =
     "The name of the command to refresh"
    command_name[autocomplete]: String,
);

pub const REFRESH_FORM_COMMAND: CommandConst = CommandConst {
    description: "Refreshes a form command",
    permissions: Permissions::MANAGE_EVENTS,
    ..command!(/refresh_form_command REFRESH_FORM_COMMAND_ARGS: refresh_command)
};

/// Re-create a form command by fetching the form definition again
async fn refresh_command(
    (command_name,): REFRESH_FORM_COMMAND_ARGS,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?;
    // load form definition
    let (form, submission_type): (String, Option<String>) = {
        let db = handler.db.lock().await;
        block_in_place(|| {
            db.conn()
            .query_row(
                "SELECT form, submission_type FROM forms WHERE guild_id = ?1 AND command_name = ?2",
                params![guild_id.get(), &command_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context(format!("Command /{} not found", command_name))
        })?
    };
    // create form command
    let form: SimpleForm = serde_json::from_slice(form.as_bytes())?;
    let params = CommandFromForm {
        command_name,
        form_id: form.id,
        submission_type,
    };
    params.add_form(handler, ctx, guild_id).await
}

args!(DELETE_FORM_COMMAND_ARGS =
     "The name of the command to delete"
    command_name[autocomplete]: String,
);

pub const DELETE_FORM_COMMAND: CommandConst = CommandConst {
    description: "Delete a form submission command",
    permissions: Permissions::MANAGE_EVENTS,
    ..command!(/delete_form_command DELETE_FORM_COMMAND_ARGS: delete_command)
};

/// Delete a form command
async fn delete_command(
    (command_name,): DELETE_FORM_COMMAND_ARGS,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?;
    // load discord command, delete it if it exists
    if let Some(cmd) = guild_id
        .get_commands(&ctx.http)
        .await?
        .iter()
        .find(|cmd| cmd.name == command_name)
    {
        guild_id.delete_command(&ctx.http, cmd.id).await?;
    };
    // delete command in database
    let db = handler.db.lock().await;
    block_in_place(|| {
        db.conn().execute(
            "DELETE FROM forms WHERE guild_id = ?1 AND command_name = ?2",
            params![guild_id.get(), &command_name],
        )
    })?;

    {
        // remove command from cache in module
        let mut forms = handler.module::<Forms>()?.forms.write().await;
        forms.retain(|form| form.command_name != command_name);
    }
    CommandResponse::public(format!("Deleted command `/{command_name}`"))
}

pub const LIST_FORMS: CommandConst = CommandConst {
    description: "List submission forms and commands",
    ..command!(/list_forms: list_forms)
};

/// returns a list of the form commands registered for this guild
async fn list_forms(
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let forms = handler.module::<Forms>()?.forms.read().await;
    let contents = forms
        .iter()
        .filter(|form| form.guild_id == guild_id)
        .map(|form| {
            format!(
                "**· [{}]({}):** </{}:{}>",
                form.form.title, form.form.responder_uri, form.command_name, form.command_id,
            )
        })
        .join("\n");
    let embed = CreateEmbed::default()
        .title("Registered forms")
        .description(contents);
    CommandResponse::public(embed)
}

args!(OVERRIDE_RANGE_ARGS =
     "The name of the command"
    command_name[autocomplete]: String,
     "The range containing the responses, e.g. \"Tab 2\"!B:F"
    range: Option<String>,
);

pub const OVERRIDE_SUBMISSION_RANGE: CommandConst = CommandConst {
    description: "To use if submissions don't go to the first tab of the linked sheet",
    permissions: Permissions::MANAGE_EVENTS,
    ..command!(/override_form_submissions_range OVERRIDE_RANGE_ARGS: override_range)
};

/// change the range of the linked spreadsheet in which to look for responses to a command
async fn override_range(
    (command_name, range): OVERRIDE_RANGE_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let module = handler.module::<Forms>()?;
    {
        // update form in cache
        let mut forms = module.forms.write().await;
        let form = forms
            .iter_mut()
            .find(|form| form.guild_id == guild_id && form.command_name == command_name)
            .ok_or_else(|| anyhow!("Command {} not found", command_name))?;
        form.submissions_range = range.clone();
    }
    // update form in DB
    let db = handler.db.lock().await;
    block_in_place(|| {
        db.conn()
            .execute(
                "UPDATE forms SET submissions_range = ?3 WHERE guild_id = ?1 AND command_name = ?2",
                params![guild_id, &command_name, range.as_deref(),],
            )
            .context("Failed to update submissions range")
    })?;
    // build response
    let range = range.as_deref().unwrap_or(DEFAULT_RANGE);
    let resp = format!("Will search for submissions to `/{command_name}` in `{range}`");
    CommandResponse::public(resp)
}

/// Queries all the forms from the database
pub fn load_forms(db: &Connection) -> anyhow::Result<Vec<FormCommand>> {
    let commands = block_in_place(|| {
        let mut stmt = db.prepare("SELECT guild_id, command_name, command_id, form, submission_type, submissions_range FROM forms")?;
        stmt.query([])?
            .map(|row| {
                let form = serde_json::from_slice(row.get::<_, String>(3)?.as_bytes()).unwrap();
                Ok(FormCommand {
                    guild_id: row.get(0)?,
                    command_name: row.get(1)?,
                    command_id: row.get(2)?,
                    form,
                    submission_type: row.get(4)?,
                    submissions_range: row.get(5)?,
                })
            })
            .collect::<Vec<_>>()
    })?;
    Ok(commands)
}

impl SimpleForm {
    pub fn responder_id(&self) -> &str {
        self.responder_uri
            .trim_start_matches("https://docs.google.com/forms/d/e/")
            .trim_end_matches("/viewform")
    }

    pub fn form_response_url(&self) -> String {
        format!(
            "https://docs.google.com/forms/u/0/d/e/{}/formResponse",
            self.responder_id()
        )
    }

    /// Handle submitting a response to a form
    pub async fn submit(
        &self,
        handler: &Handler,
        _ctx: &Context,
        interaction: &CommandInteraction,
        submission_type: &str,
    ) -> anyhow::Result<CommandResponse> {
        let user = &interaction.user;
        let user_handle = if let Some(discriminator) = user.discriminator {
            // legacy format
            format!("{}#{:04}", user.name, discriminator)
        } else {
            format!("@{}", user.name)
        };

        // get required modules
        let spotify: &Spotify = handler.module()?;
        let lookup: &AlbumLookup = handler.module()?;

        let mut song_infos = Vec::new();
        let mut song_urls = Vec::new();
        let mut value_pairs = Vec::with_capacity(self.questions.len());
        let mut next_value = None;
        for q in self.questions.iter().rev() {
            // parse hexadecimal question ID
            let question_id = u64::from_str_radix(&q.id, 16).context("Invalid form definition")?;

            // determine whether question is asking for a username
            let lowercase_title = q.title.to_lowercase();
            if lowercase_title.contains("user") || lowercase_title.contains("discord") {
                value_pairs.push((question_id, user_handle.clone()));
                continue;
            }

            // match question with command option and get its value
            let sanitized = sanitize_name(&q.title);
            let value = interaction
                .data
                .options
                .iter()
                .find(|opt| opt.name == sanitized)
                .and_then(|opt| match &opt.value {
                    CommandDataOptionValue::String(s) => Some(s.to_string()),
                    _ => None,
                })
                .or_else(|| next_value.take());
            let mut value = match value {
                Some(v) => v,
                None if q.required => {
                    bail!(
                        "Cannot submit form response: no value provided for {}",
                        q.title
                    )
                }
                None => continue,
            };

            // determine whether question is asking for a link to a song/album
            if sanitized.contains("spotify") || sanitized.contains("link") {
                if submission_type == "album" {
                    if let Some(p) = lookup.providers().iter().find(|p| p.url_matches(&value)) {
                        let album = p.get_from_url(&value).await?;
                        let album_info = album.format_name();
                        next_value = Some(album_info.to_string());
                        value = album.url.as_deref().unwrap_or_default().to_string();
                        song_infos.push(album_info)
                    }
                } else {
                    let song = spotify.get_song_from_url(&value).await?;
                    if song.duration > Duration::seconds(60 * 45) {
                        bail!("This song is too long!")
                    }
                    let song_info = format!(
                        "{} - {}",
                        Spotify::artists_to_string(&song.artists),
                        song.name,
                    );
                    next_value = Some(song_info.clone());
                    value = song.id.unwrap().url();
                    song_infos.push(song_info);
                    song_urls.push(value.to_string());
                }
            }
            value_pairs.push((question_id, value));
        }

        // build request payload
        let form_data = value_pairs
            .into_iter()
            .map(|(id, value)| format!("entry.{id}={}", urlencoding::encode(&value)))
            .join("&");

        let url = self.form_response_url();
        let resp = reqwest::Client::new()
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_data)
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("Failed to send response: status {}", resp.status());
        }

        let contents = if !song_infos.is_empty() {
            let songs = song_infos
                .iter()
                .zip(&song_urls)
                .map(|(info, url)| format!("[{info}]({url})"))
                .join(", ");
            format!("Submitted {songs} to **{}**", self.title)
        } else {
            format!("Submitted to **{}**", self.title)
        };
        CommandResponse::private(contents)
    }

    /// Fetch a user's response to this form
    pub async fn get_submissions_for_user(
        &self,
        handler: &Handler,
        user: &User,
        range: Option<&str>,
    ) -> anyhow::Result<CommandResponse> {
        let Some(sheet_id) = &self.sheet_id else {
            bail!("No linked spreadsheet, cannot check submissions");
        };
        let rows = handler
            .module::<Forms>()?
            .sheets
            .get_range(sheet_id, range.unwrap_or(DEFAULT_RANGE))
            .await?;
        let Some(values) = rows.values else {
            bail!("No submissions found on this sheet");
        };
        // find this user's submissions
        let username = user.name.to_lowercase();
        let rows = values
            .into_iter()
            .filter(|row| {
                // filter by username
                let Some(submitter) = row.first().and_then(|v| v.as_str()) else {
                    return false;
                };
                submitter
                    .trim_start_matches('@')
                    .to_lowercase()
                    .starts_with(&username)
            })
            // take the last 5 submissions
            .rev()
            .take(5)
            .map(|row| {
                row.iter()
                    .skip(1) // skip timestamp and username
                    .flat_map(|v| v.as_str())
                    .filter(|value| !(value.is_empty() || value.starts_with("https://")))
                    .join(" - ")
            })
            .collect_vec();
        // format response
        let mut resp = rows.iter().rev().join("\n");
        if resp.is_empty() {
            resp = format!(
                "No submissions from user {} to form {}",
                user.name, self.title
            );
        }
        CommandResponse::private(resp)
    }
}

args!(GET_SUBMISSIONS_ARGS =
     "the command used to submit"
    command_name[autocomplete]: String,
);

pub const GET_SUBMISSIONS: CommandConst = CommandConst {
    description: "Get your submissions to a form",
    ..command!(/get_submissions GET_SUBMISSIONS_ARGS: get_submissions)
};

/// Responds with the user's response to the specified form command
async fn get_submissions(
    (command_name,): GET_SUBMISSIONS_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    // load specified form from cache
    let forms: &Forms = handler.module()?;
    let forms = forms.forms.read().await;
    let Some(form) = forms.iter().find(|form| form.command_name == command_name) else {
        bail!("Command {command_name} not found");
    };
    form.form
        .get_submissions_for_user(handler, &command.user, form.submissions_range.as_deref())
        .await
}

/// Forms module
pub struct Forms {
    pub sheets: Sheets,
    pub forms_client: FormsClient,
    pub forms: Arc<RwLock<Vec<FormCommand>>>,
}

impl Forms {
    /// Handle completion of form management commands and form submission commands
    fn complete_forms<'a>(
        handler: &'a Handler,
        ctx: &'a Context,
        _key: CommandKey<'a>,
        ac: &'a CommandInteraction,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        async move { process_autocomplete(handler, ctx, ac).await }.boxed()
    }

    /// Handle form submission slash commands
    pub fn process_form_command<'a>(
        handler: &'a Handler,
        ctx: &'a Context,
        cmd: &'a CommandInteraction,
    ) -> BoxFuture<'a, anyhow::Result<CommandResponse>> {
        async move {
            let guild_id = cmd.guild_id()?.get();
            let data = &cmd.data;
            // find form definition
            let forms = handler.module::<Forms>()?.forms.read().await;
            let form = forms
                .iter()
                .find(|form| form.guild_id == guild_id && form.command_name == data.name);
            let Some(form) = form else {
                bail!("Command not found")
            };
            // submit response
            form.form
                .submit(handler, ctx, cmd, &form.submission_type)
                .await
        }
        .boxed()
    }
}

#[async_trait]
impl Module for Forms {
    async fn setup(&mut self, db: &mut Db) -> anyhow::Result<()> {
        block_in_place(|| {
            db.conn().execute(
                "CREATE TABLE IF NOT EXISTS forms (
                guild_id INTEGER NOT NULL,
                command_name STRING NOT NULL,
                command_id INTEGER NOT NULL,
                form STRING NOT NULL,
                submission_type STRING NOT NULL DEFAULT('song'),
                submissions_range STRING,

                UNIQUE(guild_id, command_name)
            )",
                [],
            )
        })?;
        let forms = load_forms(db.conn()).unwrap();
        *self.forms.write().await = forms;
        Ok(())
    }

    fn register_commands(&self, store: &mut dyn Storer) {
        store.register(COMMAND_FROM_FORM);
        store.register(LIST_FORMS);
        store.register(DELETE_FORM_COMMAND);
        store.register(REFRESH_FORM_COMMAND);
        store.register(GET_SUBMISSIONS);
        store.register(OVERRIDE_SUBMISSION_RANGE);

        store.register(Forms::complete_forms as CompletionHandler);
    }
}

impl RegisterableModule for Forms {
    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        builder
            .module::<Spotify>()
            .await?
            .module::<AlbumLookup>()
            .await
    }

    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        let credentials = Arc::new(google_apis::Credentials::from_file("credentials.json")?);
        let sheets = Sheets::new(&credentials);
        let forms_authenticator = credentials.authenticator(&[SCOPE_FORMS_READONLY]);
        let forms_client = FormsClient {
            authenticator: forms_authenticator,
        };
        let forms = Default::default();
        Ok(Forms {
            sheets,
            forms_client,
            forms,
        })
    }
}
