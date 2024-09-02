use std::sync::Arc;

use avail_light_core::data;
use avail_light_core::light_client::OutputEvent as LcEvent;
use avail_light_core::network::{self, p2p, rpc};
use avail_light_core::shutdown::Controller;
use avail_light_core::types::{Delay, MultiaddrConfig};
use avail_light_core::utils::spawn_in_span;
use avail_rust::kate_recovery::couscous;
use tokio::sync::{broadcast, mpsc};
use tokio_with_wasm::alias as tokio;
use tracing::{error, info, warn};
use wasm_bindgen::prelude::*;
use web_time::Duration;

#[tokio::main(flavor = "current_thread")]
#[wasm_bindgen(start)]
async fn main_js() {}

#[wasm_bindgen]
pub async fn run() {
	console_error_panic_hook::set_once();
	tracing_wasm::set_as_global_default();

	let version = clap::crate_version!();
	let shutdown = Controller::new();
	let db = data::DB::default();
	// TODO: Store and read client_id from local storage
	// let client_id = Uuid::new_v4();
	// let execution_id = Uuid::new_v4();

	let pp = Arc::new(couscous::public_params());

	let network = network::Network::Local;

	let cfg_rpc = rpc::configuration::RPCConfig {
		full_node_ws: network.full_node_ws(),
		..Default::default()
	};

	let cfg_libp2p = p2p::configuration::LibP2PConfig {
		bootstraps: vec![MultiaddrConfig::PeerIdAndMultiaddr((
			network.bootstrap_peer_id(),
			network.bootstrap_multiaddr(),
		))],
		..Default::default()
	};

	let genesis_hash = &network.genesis_hash().to_string();

	let (rpc_client, rpc_events, rpc_subscriptions) =
		rpc::init(db.clone(), genesis_hash, &cfg_rpc, shutdown.clone())
			.await
			.unwrap();

	let (id_keys, _peer_id) = p2p::identity(&cfg_libp2p, db.clone()).unwrap();

	let project_name = "avail".to_string();
	let (p2p_client, p2p_event_loop, _p2p_event_receiver) = p2p::init(
		cfg_libp2p.clone(),
		project_name,
		id_keys,
		version,
		"DEV",
		false,
		shutdown.clone(),
	)
	.await
	.unwrap();

	let network_client = network::new(p2p_client.clone(), rpc_client, pp, false);

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
	let (block_tx, _block_rx) =
		broadcast::channel::<avail_light_core::types::BlockVerified>(1 << 7);

	let channels = avail_light_core::types::ClientChannels {
		block_sender: block_tx,
		rpc_event_receiver: rpc_events.subscribe(),
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

	tokio::task::spawn(p2p_event_loop.run());

	let bootstraps = cfg_libp2p.bootstraps.clone();
	let bootstrap_p2p_client = p2p_client.clone();
	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		let bs_result = bootstrap_p2p_client
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
	}));

	if let Err(error) = light_client_handle.await {
		error!("Error running light client: {error}")
	};
}
