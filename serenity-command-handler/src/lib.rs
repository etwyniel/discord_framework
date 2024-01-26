use std::fmt::Write;
use std::{collections::HashMap, marker::PhantomData, sync::Arc, time::Instant};

use anyhow::{anyhow, bail};
use rusqlite::Connection;
use serenity::model::prelude::{GuildId, UserId};
use serenity::{
    async_trait,
    futures::future::BoxFuture,
    http::Http,
    model::application::{
        CommandDataOption, CommandDataOptionValue, CommandInteraction, Interaction,
    },
    prelude::{Context, Mutex, RwLock, TypeMap, TypeMapKey},
};
use tokio::sync::OnceCell;

use serenity_command::{CommandKey, CommandResponse};

pub mod album;
pub mod command_context;
pub mod db;
pub mod modules;

pub mod events;

use db::Db;

use command_context::Responder;

pub type CommandStore = serenity_command::CommandStore<'static, Handler>;

type SpecialCommand = for<'a> fn(
    &'a Handler,
    &'a Context,
    &'a CommandInteraction,
) -> BoxFuture<'a, anyhow::Result<CommandResponse>>;

// Format command options for debug output
fn format_options(opts: &[CommandDataOption]) -> String {
    let mut out = String::new();
    for (i, opt) in opts.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&opt.name);
        out.push_str(": ");
        match &opt.value {
            CommandDataOptionValue::String(s) => write!(&mut out, "{s:?}").unwrap(),
            val => write!(&mut out, "{val:?}").unwrap(),
        }
    }
    out
}

pub type CompletionHandler = for<'a> fn(
    handler: &'a Handler,
    ctx: &'a Context,
    key: CommandKey<'a>,
    command: &'a CommandInteraction,
) -> BoxFuture<'a, anyhow::Result<bool>>;

pub type CompletionStore = Vec<CompletionHandler>;

#[derive(Default)]
pub struct ModuleMap(TypeMap);

impl ModuleMap {
    pub fn module<M: Module>(&self) -> anyhow::Result<&M> {
        let module = self
            .0
            .get::<KeyWrapper<M>>()
            .ok_or_else(|| anyhow!("No module of type {}", std::any::type_name::<M>()))?;
        Ok(module)
    }

    pub fn module_arc<M: Module>(&self) -> anyhow::Result<Arc<M>> {
        self.0
            .get::<KeyWrapper<M>>()
            .ok_or_else(|| anyhow!("No module of type {}", std::any::type_name::<M>()))
            .map(Arc::clone)
    }

    fn add<M: Module>(&mut self, m: M) {
        self.0.insert::<KeyWrapper<M>>(Arc::new(m));
    }

    fn contains<M: Module>(&self) -> bool {
        self.0.contains_key::<KeyWrapper<M>>()
    }
}

pub trait InteractionExt {
    fn guild_id(&self) -> anyhow::Result<GuildId>;
}

impl InteractionExt for CommandInteraction {
    fn guild_id(&self) -> anyhow::Result<GuildId> {
        self.guild_id
            .ok_or_else(|| anyhow!("Must be run in a server"))
    }
}

pub struct Handler {
    pub db: Arc<Mutex<Db>>,
    pub commands: RwLock<CommandStore>,
    pub http: OnceCell<Arc<Http>>,
    pub modules: ModuleMap,
    pub special_commands: HashMap<String, SpecialCommand>,
    pub completion_handlers: CompletionStore,
    pub default_command_handler: Option<SpecialCommand>,
    pub self_id: OnceCell<UserId>,
    pub event_handlers: Arc<events::EventHandlers>,
}

impl Handler {
    pub fn builder(conn: Connection) -> HandlerBuilder {
        let db = Db { conn };
        HandlerBuilder {
            db,
            commands: Default::default(),
            modules: Default::default(),
            special_commands: Default::default(),
            completion_handlers: Default::default(),
            default_command_handler: None,
            event_handlers: events::EventHandlers::default(),
        }
    }

    pub fn module<M: Module>(&self) -> anyhow::Result<&M> {
        self.modules.module()
    }

    pub fn module_arc<M: Module>(&self) -> anyhow::Result<Arc<M>> {
        self.modules.module_arc()
    }

    async fn process_command(
        &self,
        ctx: &Context,
        cmd: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let name = cmd.data.name.as_str();
        if let Some(special) = self.special_commands.get(name) {
            return special(self, ctx, cmd).await;
        }
        let key = (name, cmd.data.kind);
        if let Some(runner) = self.commands.read().await.0.get(&key) {
            runner.run(self, ctx, cmd).await
        } else if let Some(h) = self.default_command_handler {
            return h(self, ctx, cmd).await;
        } else {
            bail!("Unknown command {name}")
        }
    }

    pub async fn process_interaction(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Autocomplete(ac) = interaction {
            let name = ac.data.name.clone();
            let key = (name.as_str(), ac.data.kind);
            for h in &self.completion_handlers {
                match h(self, &ctx, key, &ac).await {
                    Err(e) => {
                        eprintln!("Autocomplete interaction failed for command {name}: {e:?}");
                        return;
                    }
                    Ok(true) => break,
                    Ok(false) => continue,
                }
            }
            // if let Some(handler) = self.completion_handlers.get(&key) {
            //     _ = handler(self, key, ac).await;
            // }
        } else if let Interaction::Command(command) = interaction {
            // log command
            let guild_name = if let Some(guild) = command.guild_id {
                guild
                    .to_partial_guild(&ctx.http)
                    .await
                    .map(|guild| format!("[{}] ", &guild.name))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let user = &command.user.name;
            let name = &command.data.name;
            let params = format_options(&command.data.options);
            eprintln!("{guild_name}{user}: /{name} {params}");

            let start = Instant::now();
            let resp = self.process_command(&ctx, &command).await;
            let elapsed = start.elapsed();
            eprintln!(
                "{guild_name}{user}: /{name} -({:.1?})-> {:?}",
                elapsed, &resp
            );
            let resp = match resp {
                Ok(resp) => resp,
                Err(e) => CommandResponse::Private(e.to_string().into()),
            };

            if let Err(why) = command.respond(&ctx.http, resp, None).await {
                eprintln!("cannot respond to slash command: {why:?}");
                return;
            }
        }
    }
}

pub struct HandlerBuilder {
    pub db: Db,
    pub commands: CommandStore,
    pub modules: ModuleMap,
    pub special_commands: HashMap<String, SpecialCommand>,
    pub completion_handlers: CompletionStore,
    pub default_command_handler: Option<SpecialCommand>,
    pub event_handlers: events::EventHandlers
}

impl HandlerBuilder {
    pub async fn module<M: Module>(mut self) -> anyhow::Result<Self> {
        if self.modules.contains::<M>() {
            return Ok(self);
        }
        self = M::add_dependencies(self).await?;
        let mut m = M::init(&self.modules).await?;
        m.setup(&mut self.db).await?;
        m.register_commands(&mut self.commands, &mut self.completion_handlers);
        m.register_event_handlers(&mut self.event_handlers);
        self.modules.add(m);
        Ok(self)
    }

    pub async fn with_module<M: Module>(mut self, mut m: M) -> anyhow::Result<Self> {
        if self.modules.contains::<M>() {
            return Ok(self);
        }
        self = M::add_dependencies(self).await?;
        m.setup(&mut self.db).await?;
        m.register_commands(&mut self.commands, &mut self.completion_handlers);
        m.register_event_handlers(&mut self.event_handlers);
        self.modules.add(m);
        Ok(self)
    }

    pub fn default_command_handler(mut self, h: SpecialCommand) -> Self {
        self.default_command_handler = Some(h);
        self
    }

    pub fn build(self) -> Handler {
        let HandlerBuilder {
            db,
            commands,
            modules,
            special_commands,
            completion_handlers,
            default_command_handler,
            event_handlers,
        } = self;
        Handler {
            db: Arc::new(Mutex::new(db)),
            commands: RwLock::new(commands),
            http: OnceCell::new(),
            modules,
            special_commands,
            completion_handlers,
            default_command_handler,
            self_id: OnceCell::default(),
            event_handlers: Arc::new(event_handlers),
        }
    }
}

#[async_trait]
pub trait Module: 'static + Send + Sync + Sized {
    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        Ok(builder)
    }
    async fn init(m: &ModuleMap) -> anyhow::Result<Self>;
    async fn setup(&mut self, _db: &mut Db) -> anyhow::Result<()> {
        Ok(())
    }
    fn register_commands(
        &self,
        _store: &mut CommandStore,
        _completion_handlers: &mut CompletionStore,
    ) {
    }

    fn register_event_handlers(
        &self,
        _handlers: &mut events::EventHandlers,
    ) {
    }

    const AUTOCOMPLETES: &'static [&'static str] = &[];
}

pub trait ModuleKey {
    type Module: Module;
}

struct KeyWrapper<T>(PhantomData<T>);

impl<T: 'static + Send + Sync + Module> TypeMapKey for KeyWrapper<T> {
    type Value = Arc<T>;
}

pub mod prelude {
    pub use super::{
        CommandStore, CompletionStore, Handler, HandlerBuilder, InteractionExt, Module, ModuleMap,
    };
}
