use std::{ops::Deref, sync::Arc};

pub trait EventHandlerMut<T>: Send {
    fn handle(&mut self, event: &T);
}

pub trait EventHandler<T>: Send {
    fn handle(&self, event: &T);
}

impl<H, T> EventHandler<T> for Arc<H>
where
    H: EventHandler<T> + Send + Sync,
{
    fn handle(&self, event: &T) {
        self.deref().handle(event);
    }
}

pub struct EventHandlerRegistry<T> {
    mut_handlers: Vec<Box<dyn EventHandlerMut<T>>>,
    handlers: Vec<Box<dyn EventHandler<T>>>,
}

impl<T> Default for EventHandlerRegistry<T> {
    fn default() -> Self {
        Self {
            mut_handlers: Vec::new(),
            handlers: Vec::new(),
        }
    }
}

impl<T> EventHandlerRegistry<T> {
    pub fn add_mut(&mut self, handler: impl EventHandlerMut<T> + 'static) {
        self.mut_handlers.push(Box::new(handler));
    }

    pub fn add(&mut self, handler: impl EventHandler<T> + 'static) {
        self.handlers.push(Box::new(handler));
    }
}

impl<T> EventHandlerMut<T> for EventHandlerRegistry<T> {
    fn handle(&mut self, event: &T) {
        for handler in &mut self.mut_handlers {
            handler.handle(event);
        }
        for handler in &mut self.handlers {
            handler.handle(event);
        }
    }
}

impl<T, I> EventHandler<I> for T
where
    T: Fn(&I) + Send,
{
    fn handle(&self, event: &I) {
        (*self)(event)
    }
}

impl<T, I> EventHandlerMut<I> for T
where
    T: Fn(&I) + Send,
{
    fn handle(&mut self, event: &I) {
        (*self)(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn raise_with_no_handlers_does_nothing() {
        let mut registry = EventHandlerRegistry::<i32>::default();
        registry.handle(&42); // should not panic
    }

    #[test]
    fn handler_receives_all_raised_events() {
        let log: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));

        struct LogHandler(Arc<Mutex<Vec<i32>>>);
        impl EventHandlerMut<i32> for LogHandler {
            fn handle(&mut self, event: &i32) {
                self.0.lock().unwrap().push(*event);
            }
        }

        let mut registry = EventHandlerRegistry::default();
        registry.add_mut(LogHandler(Arc::clone(&log)));

        registry.handle(&1);
        registry.handle(&2);
        registry.handle(&3);

        assert_eq!(*log.lock().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn multiple_handlers_all_receive_event() {
        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        struct TaggedHandler {
            tag: &'static str,
            log: Arc<Mutex<Vec<&'static str>>>,
        }
        impl EventHandlerMut<i32> for TaggedHandler {
            fn handle(&mut self, _event: &i32) {
                self.log.lock().unwrap().push(self.tag);
            }
        }

        let mut registry = EventHandlerRegistry::default();
        registry.add_mut(TaggedHandler {
            tag: "first",
            log: Arc::clone(&log),
        });
        registry.add_mut(TaggedHandler {
            tag: "second",
            log: Arc::clone(&log),
        });
        registry.add_mut(TaggedHandler {
            tag: "third",
            log: Arc::clone(&log),
        });

        registry.handle(&0);

        assert_eq!(*log.lock().unwrap(), vec!["first", "second", "third"]);
    }
}
