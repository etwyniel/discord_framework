use futures::future::BoxFuture;
use std::boxed::Box;
use std::marker::PhantomData;
use tokio;
use typemap_rev::{TypeMap, TypeMapKey};

// Events are identified by their type (e.g. `StartPollStarted`)
// We store a map of types to list of handlers where a handler is simply a
// closure that takes a ref of the event as an argument
type Handler<E> = dyn Fn(&E) -> BoxFuture<'static, ()> + Send + Sync;

#[derive(Default)]
pub struct EventHandlers(TypeMap);

struct EventHandlerKey<E>(PhantomData<Handler<E>>);

impl<E: 'static> TypeMapKey for EventHandlerKey<E> {
    type Value = Vec<Box<Handler<E>>>;
}

impl EventHandlers {
    pub fn add_handler<E: 'static, F: Fn(&E) -> BoxFuture<'static, ()> + Send + Sync + 'static>(
        &mut self,
        handler: F,
    ) {
        let e = self.0.entry::<EventHandlerKey<E>>();
        e.or_default().push(Box::new(handler));
    }

    pub fn emit<E: Sync + Send + 'static>(&self, event: &E) {
        let Some(handlers) = self.0.get::<EventHandlerKey<E>>() else {
            return;
        };
        for h in handlers {
            tokio::spawn(h(event));
        }
    }
}
