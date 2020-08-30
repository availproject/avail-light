//! Asynchronous version of [../blocks_tree].
//!
//! Please consult the documentation of [../blocks_tree].

// TODO: rethink this module?

use super::blocks_tree;

use alloc::boxed::Box;
use core::{pin::Pin, task::Poll};
use futures::{channel::mpsc, lock::Mutex, prelude::*};

/// Configuration for the [`ChainAsync`].
pub struct Config {
    /// Configuration for the actual queue.
    pub chain_config: blocks_tree::Config,
    /// Number of elements in the queue to verify.
    pub queue_size: usize,
    /// How to spawn other background tasks.
    pub tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
}

/// Holds state about the current state of the chain for the purpose of verifying headers.
pub struct ChainAsync<T> {
    to_background: Mutex<mpsc::Sender<ToBackground<T>>>,
    from_background: Mutex<mpsc::Receiver<ToForeground<T>>>,
}

#[derive(Debug)]
enum ToBackground<T> {
    VerifyHeader {
        scale_encoded_header: Vec<u8>,
        user_data: T,
    },
    VerifyJustification {
        scale_encoded_justification: Vec<u8>,
    },
    SetFinalizedBlock {
        block_hash: [u8; 32],
    },
}

#[derive(Debug)]
enum ToForeground<T> {
    VerifyOutcome {
        scale_encoded_header: Vec<u8>,
        user_data: T,
        result: Result<VerifySuccess, blocks_tree::HeaderVerifyError>,
    },
}

impl<T> ChainAsync<T>
where
    T: Send + 'static,
{
    /// Initializes a new queue.
    pub fn new(config: Config) -> Self {
        let (to_background, mut from_foreground) = mpsc::channel(config.queue_size);
        let (mut to_foreground, from_background) = mpsc::channel(16);

        (config.tasks_executor)({
            let mut queue = blocks_tree::NonFinalizedTree::new(config.chain_config);
            Box::pin(async move {
                loop {
                    match from_foreground.next().await {
                        // Channel closed. Task end.
                        None => break,

                        Some(ToBackground::VerifyHeader {
                            scale_encoded_header,
                            user_data,
                        }) => {
                            let num = crate::header::decode(&scale_encoded_header).unwrap().number;
                            // TODO: don't clone `scale_encoded_header`
                            let outcome = queue.verify_header(scale_encoded_header.clone());
                            // TODO: remove this
                            if let Err(err) = outcome {
                                panic!("err verifying {:?} => {:?}", num, err);
                            }
                            let result = match outcome {
                                Ok(blocks_tree::HeaderVerifySuccess::Insert {
                                    insert,
                                    is_new_best,
                                    ..
                                }) => {
                                    insert.insert(());
                                    Ok(VerifySuccess { is_new_best })
                                }
                                Ok(blocks_tree::HeaderVerifySuccess::Duplicate) => {
                                    Ok(VerifySuccess { is_new_best: false })
                                } // TODO: weird
                                Err(err) => Err(err),
                            };
                            let _ = to_foreground
                                .send(ToForeground::VerifyOutcome {
                                    scale_encoded_header,
                                    user_data,
                                    result,
                                })
                                .await;
                        }
                        Some(ToBackground::VerifyJustification {
                            scale_encoded_justification,
                        }) => {
                            // TODO: don't unwrap
                            queue
                                .verify_justification(&scale_encoded_justification)
                                .unwrap()
                                .apply();
                        }
                        Some(ToBackground::SetFinalizedBlock { block_hash }) => {
                            queue.set_finalized_block(&block_hash);
                        }
                    }

                    // We do the equivalent of `std::thread::yield_now()` here to ensure that,
                    // especially in a single-threaded context, other tasks can potentially get
                    // progress.
                    // TODO: in the wasm-browser node, make sure that this doesn't slow things down too much
                    future::poll_fn({
                        let mut ready = false;
                        move |cx| {
                            if ready {
                                Poll::Ready(())
                            } else {
                                ready = true;
                                cx.waker().wake_by_ref();
                                Poll::Pending
                            }
                        }
                    })
                    .await;
                }
            })
        });

        ChainAsync {
            to_background: Mutex::new(to_background),
            from_background: Mutex::new(from_background),
        }
    }

    /// Push the given header to the queue for verification.
    ///
    /// In addition to the header to verify, an opaque `user_data` can be associated with the
    /// header and will be provided back during the feedback.
    pub async fn verify_header(&self, scale_encoded_header: Vec<u8>, user_data: T) {
        let mut to_background = self.to_background.lock().await;
        // TODO: this blocks if queue is full?
        to_background
            .send(ToBackground::VerifyHeader {
                scale_encoded_header,
                user_data,
            })
            .await
            .unwrap();
    }

    /// Push the given justificatio to the queue for verification.
    pub async fn verify_justification(&self, scale_encoded_justification: Vec<u8>) {
        let mut to_background = self.to_background.lock().await;
        // TODO: this blocks if queue is full?
        to_background
            .send(ToBackground::VerifyJustification {
                scale_encoded_justification,
            })
            .await
            .unwrap();
    }

    /// Sets the latest known finalized block. Trying to verify a block that isn't a descendant of
    /// that block will fail.
    ///
    /// The block must have been passed to [`ChainAsync::verify`].
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
        // happen for as long as the `ChainAsync` is alive.
        let event = from_background.next().await.unwrap();

        match event {
            ToForeground::VerifyOutcome {
                scale_encoded_header,
                user_data,
                result,
            } => Event::VerifyOutcome {
                scale_encoded_header,
                user_data,
                result,
            },
        }
    }
}

/// Event that happened in the verifications queue.
#[derive(Debug)]
pub enum Event<T> {
    VerifyOutcome {
        /// Copy of the header that was passed to [`ChainAsync::verify`].
        scale_encoded_header: Vec<u8>,
        /// Custom value that was passed to [`ChainAsync::verify`].
        user_data: T,
        /// Whether the block import has been successful.
        result: Result<VerifySuccess, blocks_tree::HeaderVerifyError>,
    },
}

#[derive(Debug)]
pub struct VerifySuccess {
    /// If true, the block is considered as the new best block of the chain.
    pub is_new_best: bool,
}
