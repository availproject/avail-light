use std::{
	sync::{
		atomic::{AtomicU32, Ordering},
		Arc,
	},
	time::Duration,
};

use pcap::{ConnectionStatus, Device};
use tokio::time;
use tracing::{debug, info};

// TODO: implement graceful shutdown for all async spawned functions
pub async fn start_traffic_analyzer(port: u16, sampling_interval: u64) {
	info!("Starting network analyzer.");
	let devices = Device::list().unwrap();
	let mut dev: Option<Device> = None;
	for device in devices.clone() {
		if !device.addresses.is_empty()
		// The first interface with Connected status is usually the one with all the traffic
			&& device.flags.connection_status == ConnectionStatus::Connected
		{
			dev = Some(device.clone());
		}
	}

	debug!(
		"Non lo device selected: {}",
		dev.clone().expect("device should exist").name.as_str()
	);

	// listen to loopback device for local testing
	let mut cap_loopback = pcap::Capture::from_device("lo")
		.unwrap()
		.immediate_mode(true)
		.promisc(true)
		.open()
		.unwrap();

	cap_loopback
		.filter(&format!("udp port {port}"), true)
		.unwrap();

	let mut dev_cap =
		pcap::Capture::from_device(dev.clone().expect("device should exist").name.as_str())
			.unwrap()
			.immediate_mode(true)
			.promisc(true)
			.open()
			.unwrap();
	dev_cap.filter(&format!("udp port {port}"), true).unwrap();

	let total_bytes = Arc::new(AtomicU32::new(0));

	// Start packet inspection tasks
	let total_bytes_lo = Arc::clone(&total_bytes);
	tokio::task::spawn_blocking(move || loop {
		// Start loopback listener for traffic on same machine
		match cap_loopback.next_packet() {
			Ok(packet) => {
				total_bytes_lo.fetch_add(packet.len().try_into().unwrap_or(0), Ordering::Relaxed);
			},
			Err(_) => {},
		}
	});
	let total_bytes_dev = Arc::clone(&total_bytes);
	tokio::task::spawn_blocking(move || loop {
		// Start listener for the interface facing outside network
		match dev_cap.next_packet() {
			Ok(packet) => {
				total_bytes_dev.fetch_add(packet.len().try_into().unwrap_or(0), Ordering::Relaxed);
			},
			Err(_) => {},
		}
	});

	// Spawn result handler
	tokio::task::spawn(async move {
		let mut interval = time::interval(Duration::from_secs(sampling_interval));
		loop {
			interval.tick().await;
			debug!("Total throughput: {}", total_bytes.load(Ordering::Relaxed));
			// TODO: Implement result serialization
		}
	});
}
