//! Asynchronous version of [../headers_chain_verify].
//!
//! Please consult the documentation of [../headers_chain_verify].

use super::headers_chain_verify;

use alloc::boxed::Box;
use core::pin::Pin;
use futures::{channel::mpsc, lock::Mutex, prelude::*};

/// Configuration for the [`HeadersChainVerifyAsync`].
pub struct Config {
    /// Configuration for the actual queue.
    pub chain_config: headers_chain_verify::Config,
    /// Number of elements in the queue to verify.
    pub queue_size: usize,
    /// How to spawn other background tasks.
    pub tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
}

/// Holds state about the current state of the chain for the purpose of verifying headers.
pub struct HeadersChainVerifyAsync<T> {
    to_background: Mutex<mpsc::Sender<ToBackground<T>>>,
    from_background: Mutex<mpsc::Receiver<ToForeground<T>>>,
}

#[derive(Debug)]
enum ToBackground<T> {
    Verify {
        scale_encoded_header: Vec<u8>,
        user_data: T,
    },
    SetFinalizedBlock {
        block_hash: [u8; 32],
    },
}

#[derive(Debug)]
enum ToForeground<T> {
    VerifyOutcome {
        // TODO: include result of the verification
        user_data: T,
    },
}

impl<T> HeadersChainVerifyAsync<T>
where
    T: Send + 'static,
{
    /// Initializes a new queue.
    pub fn new(config: Config) -> Self {
        let (to_background, mut from_foreground) = mpsc::channel(config.queue_size);
        let (mut to_foreground, from_background) = mpsc::channel(16);

        (config.tasks_executor)({
            let mut queue = headers_chain_verify::HeadersChainVerify::new(config.chain_config);
            Box::pin(async move {
                loop {
                    match from_foreground.next().await {
                        // Channel closed. Task end.
                        None => break,

                        Some(ToBackground::Verify {
                            scale_encoded_header,
                            user_data,
                        }) => {
                            queue.verify(scale_encoded_header);
                            let _ = to_foreground
                                .send(ToForeground::VerifyOutcome { user_data })
                                .await;
                        }
                        Some(ToBackground::SetFinalizedBlock { block_hash }) => {
                            queue.set_finalized_block(&block_hash);
                        }
                    }
                }
            })
        });

        HeadersChainVerifyAsync {
            to_background: Mutex::new(to_background),
            from_background: Mutex::new(from_background),
        }
    }

    /// Push the given header to the queue for verification.
    ///
    /// In addition to the header to verify, an opaque `user_data` can be associated with the
    /// header and will be provided back during the feedback.
    pub async fn verify(&self, scale_encoded_header: Vec<u8>, user_data: T) {
        let mut to_background = self.to_background.lock().await;
        // TODO: this blocks if queue is full?
        to_background
            .send(ToBackground::Verify {
                scale_encoded_header,
                user_data,
            })
            .await
            .unwrap();
    }

    /// Sets the latest known finalized block. Trying to verify a block that isn't a descendant of
    /// that block will fail.
    ///
    /// The block must have been passed to [`HeadersChainVerifyAsync::verify`].
    pub async fn set_finalized_block(&self, block_hash: [u8; 32]) {
        let mut to_background = self.to_background.lock().await;
        // TODO: this blocks if queue is full?
        to_background
            .send(ToBackground::SetFinalizedBlock { block_hash })
            .await
            .unwrap();
    }

    /// Returns the next event that happened in the queue.
    ///
    /// > **Note**: While it is technically possible to have multiple simultaneous pending calls
    /// >           `next_event`, doing so would be unwise.
    pub async fn next_event(&self) -> Event<T> {
        let mut from_background = self.from_background.lock().await;
        // `unwrap` can panic iff the background task has terminated, which is never supposed to
        // happen for as long as the `HeadersChainVerifyAsync` is alive.
        let event = from_background.next().await.unwrap();

        match event {
            ToForeground::VerifyOutcome { user_data } => Event::VerifyOutcome { user_data },
        }
    }
}

pub enum Event<T> {
    VerifyOutcome {
        // TODO: include result of the verification
        user_data: T,
    },
}
