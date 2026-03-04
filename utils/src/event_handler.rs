pub trait EventHandler<T>: Send {
    fn handle(&mut self, event: &T);
}

pub struct EventHandlerRegistry<T> {
    handlers: Vec<Box<dyn EventHandler<T>>>,
}

impl<T> Default for EventHandlerRegistry<T> {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }
}

impl<T> EventHandlerRegistry<T> {
    pub fn add(&mut self, handler: impl EventHandler<T> + 'static) {
        self.handlers.push(Box::new(handler));
    }

    pub fn raise(&mut self, event: &T) {
        for handler in &mut self.handlers {
            handler.handle(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn raise_with_no_handlers_does_nothing() {
        let mut registry = EventHandlerRegistry::<i32>::default();
        registry.raise(&42); // should not panic
    }

    #[test]
    fn handler_receives_all_raised_events() {
        let log: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));

        struct LogHandler(Arc<Mutex<Vec<i32>>>);
        impl EventHandler<i32> for LogHandler {
            fn handle(&mut self, event: &i32) {
                self.0.lock().unwrap().push(*event);
            }
        }

        let mut registry = EventHandlerRegistry::default();
        registry.add(LogHandler(Arc::clone(&log)));

        registry.raise(&1);
        registry.raise(&2);
        registry.raise(&3);

        assert_eq!(*log.lock().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn multiple_handlers_all_receive_event() {
        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        struct TaggedHandler {
            tag: &'static str,
            log: Arc<Mutex<Vec<&'static str>>>,
        }
        impl EventHandler<i32> for TaggedHandler {
            fn handle(&mut self, _event: &i32) {
                self.log.lock().unwrap().push(self.tag);
            }
        }

        let mut registry = EventHandlerRegistry::default();
        registry.add(TaggedHandler {
            tag: "first",
            log: Arc::clone(&log),
        });
        registry.add(TaggedHandler {
            tag: "second",
            log: Arc::clone(&log),
        });
        registry.add(TaggedHandler {
            tag: "third",
            log: Arc::clone(&log),
        });

        registry.raise(&0);

        assert_eq!(*log.lock().unwrap(), vec!["first", "second", "third"]);
    }
}
