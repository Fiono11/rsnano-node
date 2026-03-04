pub trait EventHandler<T>: Send {
    fn handle(&mut self, event: &T);
}
