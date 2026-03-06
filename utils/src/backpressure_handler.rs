pub trait BackpressureHandlerMut<T>: Send {
    fn cool_down(&mut self);
    fn recovered(&mut self);
}

pub trait BackpressureHandler<T>: Send {
    fn cool_down(&self);
    fn recovered(&self);
}

pub struct BackpressureHandlerRegistry<T> {
    mut_handlers: Vec<Box<dyn BackpressureHandlerMut<T>>>,
    handlers: Vec<Box<dyn BackpressureHandler<T>>>,
}

impl<T> Default for BackpressureHandlerRegistry<T> {
    fn default() -> Self {
        Self {
            mut_handlers: Vec::new(),
            handlers: Vec::new(),
        }
    }
}

impl<T> BackpressureHandlerRegistry<T> {
    pub fn add_mut(&mut self, handler: impl BackpressureHandlerMut<T> + 'static) {
        self.mut_handlers.push(Box::new(handler));
    }

    pub fn add(&mut self, handler: impl BackpressureHandler<T> + 'static) {
        self.handlers.push(Box::new(handler));
    }

    pub fn cool_down(&mut self) {
        for handler in &mut self.mut_handlers {
            handler.cool_down();
        }
        for handler in &self.handlers {
            handler.cool_down();
        }
    }

    pub fn recovered(&mut self) {
        for handler in &mut self.mut_handlers {
            handler.recovered();
        }
        for handler in &self.handlers {
            handler.recovered();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn no_handlers_does_nothing() {
        let mut registry = BackpressureHandlerRegistry::<()>::default();
        registry.cool_down();
        registry.recovered();
    }

    #[test]
    fn mut_handler_receives_cool_down_and_recovered() {
        let log = Log::default();
        let mut registry = BackpressureHandlerRegistry::default();
        registry.add_mut(LogHandler(log.clone()));

        registry.cool_down();
        registry.recovered();

        assert_eq!(log.read(), vec!["cool_down", "recovered"]);
    }

    #[test]
    fn multiple_handlers_all_receive_signals() {
        let log = Log::default();
        let mut registry = BackpressureHandlerRegistry::default();
        registry.add_mut(TaggedHandler {
            tag: "first",
            log: log.clone(),
        });
        registry.add_mut(TaggedHandler {
            tag: "second",
            log: log.clone(),
        });
        registry.add_mut(TaggedHandler {
            tag: "third",
            log: log.clone(),
        });

        registry.cool_down();

        assert_eq!(log.read(), vec!["first", "second", "third"]);
    }

    #[test]
    fn immutable_handler_receives_signals() {
        let log = Log::default();
        let mut registry = BackpressureHandlerRegistry::default();
        registry.add(LogHandler(log.clone()));

        registry.cool_down();
        registry.recovered();

        assert_eq!(log.read(), vec!["cool_down", "recovered"]);
    }

    /* Test helpers */

    #[derive(Default, Clone)]
    struct Log(Arc<Mutex<Vec<&'static str>>>);

    impl Log {
        fn add(&self, message: &'static str) {
            self.0.lock().unwrap().push(message);
        }

        fn read(&self) -> Vec<&'static str> {
            self.0.lock().unwrap().clone()
        }
    }

    struct LogHandler(Log);

    impl BackpressureHandlerMut<()> for LogHandler {
        fn cool_down(&mut self) {
            self.0.add("cool_down");
        }
        fn recovered(&mut self) {
            self.0.add("recovered");
        }
    }

    impl BackpressureHandler<()> for LogHandler {
        fn cool_down(&self) {
            self.0.add("cool_down");
        }
        fn recovered(&self) {
            self.0.add("recovered");
        }
    }

    struct TaggedHandler {
        tag: &'static str,
        log: Log,
    }

    impl BackpressureHandlerMut<()> for TaggedHandler {
        fn cool_down(&mut self) {
            self.log.add(self.tag);
        }
        fn recovered(&mut self) {}
    }
}
