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
    mut_handlers: Vec<(&'static str, Box<dyn EventHandlerMut<T>>)>,
    handlers: Vec<(&'static str, Box<dyn EventHandler<T>>)>,
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
    pub fn add_mut<H: EventHandlerMut<T> + 'static>(&mut self, handler: H) {
        self.mut_handlers
            .push((std::any::type_name::<H>(), Box::new(handler)));
    }

    pub fn add<H: EventHandler<T> + 'static>(&mut self, handler: H) {
        self.handlers
            .push((std::any::type_name::<H>(), Box::new(handler)));
    }

    /// Hands the event to every handler, calling `handled` with the type
    /// name of each handler once it is done, so that callers can time them
    pub fn handle_each(&mut self, event: &T, mut handled: impl FnMut(&'static str)) {
        for (name, handler) in &mut self.mut_handlers {
            handler.handle(event);
            handled(name);
        }
        for (name, handler) in &mut self.handlers {
            handler.handle(event);
            handled(name);
        }
    }
}

impl<T> EventHandlerMut<T> for EventHandlerRegistry<T> {
    fn handle(&mut self, event: &T) {
        self.handle_each(event, |_| {});
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
    fn each_handler_is_reported_by_type_name_after_it_ran() {
        struct First;
        impl EventHandler<i32> for First {
            fn handle(&self, _event: &i32) {}
        }
        struct Second;
        impl EventHandlerMut<i32> for Second {
            fn handle(&mut self, _event: &i32) {}
        }
        let mut registry = EventHandlerRegistry::<i32>::default();
        registry.add(First);
        registry.add_mut(Second);

        let mut handled = Vec::new();
        registry.handle_each(&1, |name| handled.push(name));

        assert_eq!(handled.len(), 2);
        assert!(handled[0].ends_with("Second"));
        assert!(handled[1].ends_with("First"));
    }

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
