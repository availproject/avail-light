
pub struct Subscriptions<T> {
    runtime_version_subscriptions: Vec<T>,
}

impl<T> Subscriptions<T> {
    pub fn new() -> Self {
        Subscriptions {
            runtime_version_subscriptions: Vec::new(),
        }
    }

    pub fn notify_runtime_version(&mut self, new_version: u32) {
        
    }
}
