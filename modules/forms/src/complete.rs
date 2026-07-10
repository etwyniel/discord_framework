use std::borrow::Borrow;

use serenity::all::{AutocompleteChoice, CommandInteraction};
use serenity::builder::{CreateAutocompleteResponse, CreateInteractionResponse};
use serenity::model::prelude::UserId;

use serenity::prelude::Context;
use serenity_command_handler::album::AlbumProvider;
use serenity_command_handler::command_context::{get_focused_option, get_str_opt_ac};
use serenity_command_handler::prelude::*;
use spotify::rspotify::clients::BaseClient;

type Spotify = spotify::Spotify<spotify::rspotify::ClientCredsSpotify>;

use super::{
    DELETE_FORM_COMMAND, Forms, GET_SUBMISSIONS, OVERRIDE_SUBMISSION_RANGE, REFRESH_FORM_COMMAND,
};
use spotify_activity::SpotifyActivity;

#[derive(Eq, PartialEq)]
enum CompletionType {
    Albums,
    Songs,
}

/// Get the name and URL of the spotify track a user is currently listening to (if any).
async fn get_now_playing(
    handler: &Handler,
    user_id: UserId,
) -> anyhow::Result<Option<(String, String)>> {
    // get both required modules
    let spotify: &Spotify = handler.module()?;
    let activity: &SpotifyActivity = handler.module()?;

    let Some(np) = activity.user_now_playing(user_id).await else {
        // no spotify activity for this user
        return Ok(None);
    };
    // fetch track info
    let track = spotify.client.track(np.clone(), None).await?;
    let name = format!(
        "{} - {}",
        Spotify::artists_to_string(&track.artists),
        track.name
    );
    let url = format!(
        "https://open.spotify.com/track/{}",
        Borrow::<str>::borrow(&np)
    );
    Ok(Some((name, url)))
}

/// Fetch completion suggestions for a track or album link using spotify.
async fn autocomplete_link(
    handler: &Handler,
    user_id: UserId,
    option: &str,
    ty: CompletionType,
) -> Vec<(String, String)> {
    let spotify: &Spotify = handler.module().unwrap();
    if option.is_empty() && ty == CompletionType::Songs {
        // nothing typed yet, suggest the user's current track (if any)
        match get_now_playing(handler, user_id).await {
            Ok(np) => return np.into_iter().collect(),
            Err(e) => {
                eprintln!("Error getting user's current track: {e}");
                return Vec::new();
            }
        }
    }
    // if current query is too short or if it is a URL, suggest nothing
    if option.len() >= 5 && !(option.starts_with("https://") || option.starts_with("http://")) {
        match ty {
            CompletionType::Albums => spotify.query_albums(option).await,
            CompletionType::Songs => spotify.query_songs(option).await,
        }
        .unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// Process an autocomplete request for a form command.
pub async fn process_autocomplete(
    handler: &Handler,
    ctx: &Context,
    ac: &CommandInteraction,
) -> anyhow::Result<bool> {
    let guild_id = ac.guild_id()?.get();
    let choices: Vec<_>;
    let options = &ac.data.options;
    let forms: &Forms = handler.module()?;
    let cmd_name = ac.data.name.as_str();
    let focused = match get_focused_option(options) {
        Some(opt) => opt,
        None => return Ok(true),
    };
    // list of commands that take a form command as an argument
    const FORM_COMMANDS: [&str; 4] = [
        DELETE_FORM_COMMAND.name,
        REFRESH_FORM_COMMAND.name,
        GET_SUBMISSIONS.name,
        OVERRIDE_SUBMISSION_RANGE.name,
    ];
    if FORM_COMMANDS.contains(&cmd_name) {
        // this is a command that operates on forms,
        // complete with registered commands in this guild
        let opt = get_str_opt_ac(options, "command_name").unwrap_or_default();
        choices = forms
            .forms
            .read()
            .await
            .iter()
            .map(|form| (form.guild_id, &form.command_name))
            .filter(|(guild, name)| *guild == guild_id && name.contains(opt))
            .map(|(_, name)| (name.to_string(), name.to_string()))
            .collect();
    } else {
        // load list of forms to check if this is a form command
        let forms = forms.forms.read().await;
        let form = forms
            .iter()
            .find(|form| form.guild_id == guild_id && form.command_name == cmd_name);
        let Some(form) = form else { return Ok(false) };
        // this is a command that submits to a form,
        // offer completion for song/album fields
        if !(focused.contains("spotify") || focused.contains("link")) {
            // no completion for this field, but we did handle completion
            return Ok(true);
        }
        let Some(val) = get_str_opt_ac(options, focused) else {
            // field not found, should not happen but return Ok
            return Ok(true);
        };
        let ty = match form.submission_type.as_str() {
            "album" => CompletionType::Albums,
            _ => CompletionType::Songs,
        };
        choices = autocomplete_link(handler, ac.user.id, val, ty).await;
    }
    // respond to completion interaction
    let resp =
        choices
            .into_iter()
            .fold(CreateAutocompleteResponse::new(), |resp, (name, value)| {
                let len = 100.min(name.len());
                resp.add_choice(AutocompleteChoice::new(name[..len].to_string(), value))
            });
    ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(resp))
        .await?;
    Ok(true)
}
