use std::fmt::Write;
use std::{collections::HashMap, marker::PhantomData, sync::Arc, time::Instant};

use anyhow::{anyhow, bail};
use bot_management::ModManagement;
use rusqlite::Connection;
pub use serenity; // re-export
use serenity::all::{ComponentInteraction, ModalInteraction};
use serenity::model::prelude::{GuildId, UserId};
use serenity::{
    async_trait,
    futures::future::BoxFuture,
    http::Http,
    model::application::{
        CommandDataOption, CommandDataOptionValue, CommandInteraction, Interaction,
    },
    prelude::{Context, Mutex, RwLock},
};
use tokio::sync::OnceCell;
use typemap_rev::{TypeMap, TypeMapKey};

use serenity_command::{CommandKey, CommandResponse};

pub mod album;
pub mod bot_management;
pub mod command_context;
pub mod db;
pub mod events;
pub mod modules;

use db::Db;

use command_context::Responder;

pub type CommandConst = serenity_command::CommandConst<Handler>;
pub type CommandStore = serenity_command::CommandStore<'static, Handler>;
pub type ModalCommandConst = serenity_command::ModalCommandConst<Handler>;
pub type ModalCommandStore = serenity_command::ModalCommandStore<'static, Handler>;
pub type ComponentCommandConst = serenity_command::ComponentCommandConst<Handler>;
pub type ComponentCommandStore = serenity_command::ComponentCommandStore<'static, Handler>;

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

    fn add<M: Module>(&mut self, m: Arc<M>) {
        self.0.insert::<KeyWrapper<M>>(m);
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
    pub management_guild: GuildId,
    pub commands: RwLock<CommandStore>,
    pub modal_commands: RwLock<ModalCommandStore>,
    pub component_commands: RwLock<ComponentCommandStore>,
    pub http: OnceCell<Arc<Http>>,
    pub modules: ModuleMap,
    pub module_list: Vec<Arc<dyn Module>>,
    pub special_commands: HashMap<String, SpecialCommand>,
    pub completion_handlers: CompletionStore,
    pub default_command_handler: Option<SpecialCommand>,
    pub self_id: OnceCell<UserId>,
    pub event_handlers: Arc<events::EventHandlers>,
}

impl Handler {
    pub async fn builder(conn: Connection, management_guild: GuildId) -> HandlerBuilder {
        let db = Db::new(conn).unwrap();
        let mut builder = HandlerBuilder {
            db,
            management_guild,
            commands: Default::default(),
            modal_commands: Default::default(),
            component_commands: Default::default(),
            modules: Default::default(),
            module_list: Default::default(),
            special_commands: Default::default(),
            completion_handlers: Default::default(),
            default_command_handler: None,
            event_handlers: events::EventHandlers::default(),
        };
        // register default module(s)
        builder = builder.module::<ModManagement>().await.unwrap();
        builder
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
            (runner.run)(self, ctx, cmd).await
        } else if let Some(h) = self.default_command_handler {
            return h(self, ctx, cmd).await;
        } else {
            bail!("Unknown command {name}")
        }
    }

    async fn process_modal(
        &self,
        ctx: &Context,
        modal: &ModalInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let name = modal.data.custom_id.as_str();
        if let Some(command) = self.modal_commands.read().await.0.get(name) {
            (command.run)(self, ctx, modal).await
        } else {
            bail!("Uknown modal interaction '{name}'")
        }
    }

    async fn process_component(
        &self,
        ctx: &Context,
        component: &ComponentInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let name = component.data.custom_id.as_str();
        if let Some(command) = self.component_commands.read().await.0.get(name) {
            (command.run)(self, ctx, component).await
        } else {
            bail!("Uknown component interaction '{name}'")
        }
    }

    pub async fn process_interaction(&self, ctx: &Context, interaction: &Interaction) {
        if let Interaction::Autocomplete(ac) = interaction {
            let name = ac.data.name.clone();
            let key = (name.as_str(), ac.data.kind);
            for h in &self.completion_handlers {
                match h(self, ctx, key, ac).await {
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
            let resp = self.process_command(ctx, command).await;
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
            }
        } else if let Interaction::Modal(modal) = interaction {
            // log command
            let guild_name = if let Some(guild) = modal.guild_id {
                guild
                    .to_partial_guild(&ctx.http)
                    .await
                    .map(|guild| format!("[{}] ", &guild.name))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let user = &modal.user.name;
            let name = &modal.data.custom_id;
            eprintln!("{guild_name}{user}: |>{name}");
            let start = Instant::now();
            let resp = self.process_modal(ctx, modal).await;
            let elapsed = start.elapsed();
            eprintln!(
                "{guild_name}{user}: |>{name} -({:.1?})-> {:?}",
                elapsed, &resp
            );
            let resp = match resp {
                Ok(resp) => resp,
                Err(e) => CommandResponse::Private(e.to_string().into()),
            };

            if let Err(why) = modal.respond(&ctx.http, resp, None).await {
                eprintln!("cannot respond to modal command: {why:?}");
            }
        } else if let Interaction::Component(component) = interaction {
            // log command
            let guild_name = if let Some(guild) = component.guild_id {
                guild
                    .to_partial_guild(&ctx.http)
                    .await
                    .map(|guild| format!("[{}] ", &guild.name))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let user = &component.user.name;
            let name = &component.data.custom_id;
            eprintln!("{guild_name}{user}: [>{name}");
            let start = Instant::now();
            let resp = self.process_component(ctx, component).await;
            let elapsed = start.elapsed();
            eprintln!(
                "{guild_name}{user}: [>{name} -({:.1?})-> {:?}",
                elapsed, &resp
            );
            let resp = match resp {
                Ok(resp) => resp,
                Err(e) => CommandResponse::Private(e.to_string().into()),
            };

            if let Err(why) = component.respond(&ctx.http, resp, None).await {
                eprintln!("cannot respond to component command: {why:?}");
            }
        }
    }
}

// pub trait CommandStorer {
//     fn register(&mut self, command: CommandConst);
// }
//
// pub trait ModalCommandStorer {
//     fn register(&mut self, command: ModalCommandConst);
// }
//
// pub trait ComponentCommandStorer {
//     fn register(&mut self, command: ComponentCommandConst);
// }
//
// pub trait CompletionStorer {
//     fn register(&mut self, completion: CompletionHandler);
// }

pub trait TStorer<T> {
    fn register(&mut self, val: T);
}

pub struct HandlerBuilder {
    pub db: Db,
    pub management_guild: GuildId,
    pub commands: CommandStore,
    pub modal_commands: ModalCommandStore,
    pub component_commands: ComponentCommandStore,
    pub modules: ModuleMap,
    pub module_list: Vec<Arc<dyn Module>>,
    pub special_commands: HashMap<String, SpecialCommand>,
    pub completion_handlers: CompletionStore,
    pub default_command_handler: Option<SpecialCommand>,
    pub event_handlers: events::EventHandlers,
}

impl TStorer<CommandConst> for HandlerBuilder {
    fn register(&mut self, command: CommandConst) {
        self.commands.register(command);
    }
}

impl TStorer<ModalCommandConst> for HandlerBuilder {
    fn register(&mut self, command: ModalCommandConst) {
        self.modal_commands.register(command);
    }
}

impl TStorer<ComponentCommandConst> for HandlerBuilder {
    fn register(&mut self, command: ComponentCommandConst) {
        self.component_commands.register(command);
    }
}

impl TStorer<CompletionHandler> for HandlerBuilder {
    fn register(&mut self, completion: CompletionHandler) {
        self.completion_handlers.push(completion);
    }
}

pub trait Storer:
    TStorer<CommandConst>
    + TStorer<ModalCommandConst>
    + TStorer<ComponentCommandConst>
    + TStorer<CompletionHandler>
{
}

impl Storer for HandlerBuilder {}

impl HandlerBuilder {
    pub async fn module<M: RegisterableModule>(mut self) -> anyhow::Result<Self> {
        if self.modules.contains::<M>() {
            return Ok(self);
        }
        self = M::add_dependencies(self).await?;
        let mut m = M::init(&self.modules).await?;
        m.setup(&mut self.db).await?;
        m.register_commands(&mut self);
        let module: Arc<M> = m.into();
        Arc::clone(&module).register_event_handlers(&mut self.event_handlers);
        self.modules.add(Arc::clone(&module));
        self.module_list.push(module.as_trait());
        Ok(self)
    }

    pub async fn with_module<M: RegisterableModule>(mut self, mut m: M) -> anyhow::Result<Self> {
        if self.modules.contains::<M>() {
            return Ok(self);
        }
        self = M::add_dependencies(self).await?;
        m.setup(&mut self.db).await?;
        m.register_commands(&mut self);
        let module: Arc<M> = m.into();
        Arc::clone(&module).register_event_handlers(&mut self.event_handlers);
        self.modules.add(Arc::clone(&module));
        self.module_list.push(module.as_trait());
        Ok(self)
    }

    pub fn default_command_handler(mut self, h: SpecialCommand) -> Self {
        self.default_command_handler = Some(h);
        self
    }

    pub fn build(self) -> Handler {
        let HandlerBuilder {
            db,
            management_guild,
            commands,
            modal_commands,
            component_commands,
            modules,
            module_list,
            special_commands,
            completion_handlers,
            default_command_handler,
            event_handlers,
        } = self;
        Handler {
            db: Arc::new(Mutex::new(db)),
            management_guild,
            commands: RwLock::new(commands),
            modal_commands: RwLock::new(modal_commands),
            component_commands: RwLock::new(component_commands),
            http: OnceCell::new(),
            modules,
            module_list,
            special_commands,
            completion_handlers,
            default_command_handler,
            self_id: OnceCell::default(),
            event_handlers: Arc::new(event_handlers),
        }
    }
}

#[async_trait]
pub trait Module: 'static + Send + Sync {
    async fn setup(&mut self, _db: &mut Db) -> anyhow::Result<()> {
        Ok(())
    }
    fn register_commands(&self, _store: &mut dyn Storer) {}

    fn register_event_handlers(self: Arc<Self>, _handlers: &mut events::EventHandlers) {}

    fn autocompletes(&self) -> &'static [&'static str] {
        &[]
    }

    fn start(&self, _ctx: &Context, _data_about_bot: &serenity::model::gateway::Ready) {}
}

#[allow(async_fn_in_trait)]
pub trait RegisterableModule: Module + Sized {
    async fn init(m: &ModuleMap) -> anyhow::Result<Self>;

    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        Ok(builder)
    }

    fn as_trait(self: Arc<Self>) -> Arc<dyn Module> {
        self
    }
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
        CommandConst, CommandStore, CompletionHandler, CompletionStore, Handler, HandlerBuilder,
        InteractionExt, ModalCommandStore, Module, ModuleMap, RegisterableModule, Storer,
    };
}
