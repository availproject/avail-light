
/// State machine representing the network currently running.
pub struct Network {}

/// Event that can happen on the network.
#[derive(Debug)]
pub enum Event {
    /// Received a block announcement for a block we've never heard of before.
    BlockAnnouncementReceived {},
}

/// Configuration for starting the network.
///
/// Internal to the `network` module.
pub(super) struct Config {

}

impl Network {
    pub(super) fn start(config: Config) -> Self {
        Network {}
    }

    /// Sends out an announcement about the given block.
    pub async fn announce_block(&mut self) {}

    /// Returns the next event that happened on the network.
    pub async fn next_event(&mut self) -> Event {
        loop {
            futures::pending!()
        }
    }
}
