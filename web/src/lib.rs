#![cfg(target_arch = "wasm32")]
use avail_light_core::api::configuration::SharedConfig;
use avail_light_core::api::types::{PublishMessage, Request, Topic};
use avail_light_core::api::v2::transactions::Submitter;
use avail_light_core::light_client::{self, OutputEvent as LcEvent};
use avail_light_core::network::{self, p2p, rpc, Network};
use avail_light_core::shutdown::Controller;
use avail_light_core::types::{Delay, NetworkMode, PeerAddress};
use avail_light_core::utils::spawn_in_span;
use avail_light_core::{api, data};
use avail_rust::SDK;
use clap::ValueEnum;
use kate::couscous;
use libp2p::Multiaddr;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::OnceCell;
use tokio::sync::{
	broadcast,
	mpsc::{self, UnboundedSender},
	Mutex,
};
use tokio_with_wasm::alias as tokio;
use tracing::{error, info, warn};
use wasm_bindgen::prelude::*;
use web_sys::js_sys;
use web_time::Duration;
use web_time::Instant;

#[tokio::main(flavor = "current_thread")]
#[wasm_bindgen(start)]
async fn main_js() {}

static SENDER: OnceCell<UnboundedSender<String>> = OnceCell::const_new();

async fn set_sender(sender: UnboundedSender<String>) {
	SENDER.set(sender).unwrap();
}

#[wasm_bindgen]
pub fn post_message(message: String) {
	if let Some(sender) = SENDER.get() {
		sender.send(message).unwrap();
	}
}

fn send_message_to_browser(message: &str) {
	let worker_scope = js_sys::global();
	worker_scope
		.dyn_ref::<web_sys::DedicatedWorkerGlobalScope>()
		.expect("Should be running in a Web Worker")
		.post_message(&JsValue::from_str(message))
		.expect("Failed to post message");
}

#[wasm_bindgen]
pub async fn run(network_param: Option<String>, bootstrap_param: Option<String>) {
	console_error_panic_hook::set_once();
	tracing_wasm::set_as_global_default();

	let mut network = network::Network::Local;
	if let Some(value) = network_param {
		network = Network::from_str(&value, true).unwrap();
	};

	let mut bootstrap_multiaddr = network.bootstrap_multiaddr();
	if let Some(value) = bootstrap_param {
		bootstrap_multiaddr = Multiaddr::from_str(&value).unwrap();
	};

	let version = clap::crate_version!();
	let shutdown = Controller::new();
	let db = data::DB::default();

	// TODO: Store and read client_id from local storage
	// let client_id = Uuid::new_v4();
	// let execution_id = Uuid::new_v4();

	let pp = Arc::new(couscous::multiproof_params());

	let cfg_rpc = rpc::configuration::RPCConfig {
		full_node_ws: network.full_node_ws(),
		..Default::default()
	};

	let cfg_libp2p = p2p::configuration::LibP2PConfig {
		bootstraps: vec![PeerAddress::PeerIdAndMultiaddr((
			network.bootstrap_peer_id(),
			bootstrap_multiaddr,
		))],
		..Default::default()
	};

	let genesis_hash = &network.genesis_hash().to_string();
	let (rpc_event_sender, rpc_event_receiver) = broadcast::channel(1000);
	let mut publish_rpc_event_receiver = rpc_event_sender.subscribe();

	let (rpc_client, rpc_subscriptions) = rpc::init(
		db.clone(),
		genesis_hash,
		&cfg_rpc,
		shutdown.clone(),
		rpc_event_sender.clone(),
	)
	.await
	.unwrap();

	let (id_keys, _peer_id) = p2p::identity(&cfg_libp2p, db.clone()).unwrap();

	let (p2p_client, _p2p_event_loop, _p2p_event_receiver) = p2p::init(
		cfg_libp2p.clone(),
		Default::default(),
		id_keys,
		version,
		"DEV",
		false,
		shutdown.clone(),
	)
	.await
	.unwrap();

	let p2p_client = Arc::new(Mutex::new(Some(p2p_client)));
	let network_client = network::new(
		p2p_client.clone(),
		rpc_client,
		pp,
		NetworkMode::RPCOnly,
		false,
	);

	// spawn the RPC Network task for Event Loop to run in the background
	// and shut it down, without delays
	let _rpc_subscriptions_handle = spawn_in_span(shutdown.with_cancel(shutdown.with_trigger(
		"Subscription loop failure triggered shutdown".to_string(),
		async {
			let result = rpc_subscriptions.run().await;
			if let Err(ref err) = result {
				error!(%err, "Subscription loop ended with error");
			};
			result
		},
	)));

	let (lc_sender, mut lc_receiver) = mpsc::unbounded_channel::<LcEvent>();
	let (block_tx, mut block_rx) =
		broadcast::channel::<avail_light_core::types::BlockVerified>(1 << 7);

	let channels = avail_light_core::types::ClientChannels {
		block_sender: block_tx,
		rpc_event_receiver,
	};

	spawn_in_span(async move {
		loop {
			let Some(message) = lc_receiver.recv().await else {
				info!("Exiting...");
				break;
			};
			info!("{message:?}");
		}
	});

	let light_client_handle = tokio::task::spawn(avail_light_core::light_client::run(
		db.clone(),
		network_client,
		99.9,
		Delay(Some(Duration::from_secs(20))),
		channels,
		shutdown.clone(),
		lc_sender,
	));

	let topic = Topic::HeaderVerified;
	spawn_in_span(shutdown.with_cancel(async move {
		loop {
			let message = match publish_rpc_event_receiver.recv().await {
				Ok(value) => value,
				Err(error) => {
					error!(?topic, "Cannot receive message: {error}");
					return;
				},
			};
			let message: Option<PublishMessage> = match message.try_into() {
				Ok(Some(message)) => Some(message),
				Ok(None) => continue, // Silently skip
				Err(error) => {
					error!(?topic, "Cannot create message: {error}");
					continue;
				},
			};

			let message = serde_json::to_string(&message).unwrap();
			send_message_to_browser(&message)
		}
	}));

	let topic = Topic::ConfidenceAchieved;
	spawn_in_span(shutdown.with_cancel(async move {
		loop {
			let message = match block_rx.recv().await {
				Ok(value) => value,
				Err(error) => {
					error!(?topic, "Cannot receive message: {error}");
					return;
				},
			};
			let message: Option<PublishMessage> = match message.try_into() {
				Ok(Some(message)) => Some(message),
				Ok(None) => continue, // Silently skip
				Err(error) => {
					error!(?topic, "Cannot create message: {error}");
					continue;
				},
			};

			let message = serde_json::to_string(&message).unwrap();
			send_message_to_browser(&message)
		}
	}));

	// tokio::task::spawn(_p2p_event_loop.run());

	let bootstraps = cfg_libp2p.bootstraps.clone();
	let bootstrap_p2p_client = p2p_client.clone();
	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		let client_guard = bootstrap_p2p_client.lock().await;
		if let Some(p2p_client) = client_guard.as_ref() {
			let bs_result = p2p_client
				.clone()
				.bootstrap_on_startup(&bootstraps)
				.await;
			match bs_result {
				Ok(_) => {
					info!("Bootstrap done.");
				},
				Err(e) => {
					warn!("Bootstrap process: {e:?}.");
				},
			}
		} else {
			warn!("No P2P client available for bootstrap");
		}
	}));

	let (sender, mut receiver) = mpsc::unbounded_channel::<String>();
	set_sender(sender).await;

	let config = SharedConfig::default();

	spawn_in_span(async move {
		loop {
			if let Some(message) = receiver.recv().await {
				info!("Received message: {message}");
				let request: Request = match serde_json::from_str(&message) {
					Ok(request) => request,
					Err(error) => {
						error!("Failed to parse request: {error}");
						continue;
					},
				};

				let Ok(response) = api::v2::messages::handle_request(
					request,
					version,
					&config,
					None::<Arc<Submitter<data::DB>>>,
					db.clone(),
				)
				.await
				else {
					continue;
				};
				send_message_to_browser(&serde_json::to_string(&response).unwrap());
			}
		}
	});

	if let Err(error) = light_client_handle.await {
		error!("Error running light client: {error}")
	};
}

/// Returns the latest block hash and confidence.
/// NOTE: Function will panic in case of errors,
/// until better API for error reporting is implemented
#[wasm_bindgen]
pub async fn latest_block(network_param: Option<String>) -> String {
	console_error_panic_hook::set_once();
	if !tracing::dispatcher::has_been_set() {
		tracing_wasm::set_as_global_default();
	}

	let mut network = network::Network::Local;
	if let Some(value) = network_param {
		network = Network::from_str(&value, true).unwrap();
	};

	let endpoint = network.full_node_ws().first().cloned().unwrap();
	let sdk = SDK::new(&endpoint).await.unwrap();
	let db = data::DB::default();
	let shutdown = Controller::new();
	let pp = Arc::new(couscous::multiproof_params());

	info!("Fetching the latest block...");

	let block = sdk.client.online_client.blocks().at_latest().await.unwrap();
	let header = block.header().clone();
	let received_at = Instant::now();

	let (lc_sender, mut lc_receiver) = mpsc::unbounded_channel::<LcEvent>();

	spawn_in_span(async move {
		loop {
			let Some(message) = lc_receiver.recv().await else {
				info!("Exiting...");
				break;
			};
			info!("{message:?}");
		}
	});

	let cfg_rpc = rpc::configuration::RPCConfig {
		full_node_ws: network.full_node_ws(),
		..Default::default()
	};

	let (rpc_event_sender, mut rpc_event_receiver) = broadcast::channel(1000);

	let (rpc_client, _) = rpc::init(
		db.clone(),
		network.genesis_hash(),
		&cfg_rpc,
		shutdown,
		rpc_event_sender,
	)
	.await
	.unwrap();

	spawn_in_span(async move {
		loop {
			match rpc_event_receiver.recv().await {
				Ok(_) => break,
				Err(error) => error!("{error:?}"),
			}
		}
	});

	let network_client = network::new_rpc(rpc_client, pp);

	let confidence =
		light_client::process_block(db, &network_client, 99.9, header, received_at, lc_sender)
			.await
			.unwrap()
			.map(|confidence| format!("{confidence}"))
			.unwrap_or("none".to_string());

	format!("Confidence for block {} is {confidence}.", block.number())
}
