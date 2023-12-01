use color_eyre::Result;
use pcap::{Active, Capture, ConnectionStatus, Device};
use std::{
	sync::{
		atomic::{AtomicU32, Ordering},
		Arc,
	},
	time::Duration,
};
use tokio::time;
use tracing::{debug, error, info, warn};

// TODO: implement graceful shutdown for all async spawned functions
pub async fn start_traffic_analyzer(port: u16, sampling_interval: u64) {
	let mut is_one_capture_active = false;
	info!("Starting network analyzer.");
	let devices = match Device::list() {
		Ok(dev) => dev,
		Err(err) => {
			error!("Unable to retrieve NIC list: {}", err);
			// If no device list is present, there's no point in going forward
			return;
		},
	};
	let mut dev: Option<Device> = None;
	for device in devices {
		if !device.addresses.is_empty()
		// The first interface with Connected status is usually the one with all the traffic
			&& device.flags.connection_status == ConnectionStatus::Connected
		{
			dev = Some(device.clone());
		}
	}

	let total_bytes = Arc::new(AtomicU32::new(0));

	// Listen to loopback device for local testing
	if start_listening_on_device("lo".to_owned(), port, Arc::clone(&total_bytes)).is_ok() {
		is_one_capture_active = true;
	}

	// Listen to non-loopback device for local testing
	if let Some(device) = dev {
		debug!("Non lo device selected: {}", device.name.as_str());
		if start_listening_on_device(device.name, port, Arc::clone(&total_bytes)).is_ok() {
			is_one_capture_active = true;
		}
	};

	// Spawn result handler
	if is_one_capture_active {
		tokio::task::spawn(async move {
			let mut interval = time::interval(Duration::from_secs(sampling_interval));
			loop {
				interval.tick().await;
				info!("Total throughput: {}", total_bytes.load(Ordering::Relaxed));
				// TODO: Implement result serialization
			}
		});
	} else {
		warn!("No interfaces can be listened on. Exiting network analyzer...");
	}
}

fn start_listening_on_device(
	device_name: String,
	port: u16,
	total_bytes: Arc<AtomicU32>,
) -> Result<()> {
	let capture = open_capture_from_device(device_name)
		.map_err(|err| {
			error!(
				"Unable to activate capture for non-loopback device: {}",
				err
			);
		})
		.and_then(|mut capture| {
			capture
				.filter(&format!("udp port {port}"), true)
				.map_err(|err| error!("Unable to set filter to loopback interface: {}", err))
				.map(|_| capture)
		});

	if let Ok(mut capture) = capture {
		debug!("Loopback interface filtering set");
		// Start listener for the interface facing outside network
		let total_bytes_dev = Arc::clone(&total_bytes);
		tokio::task::spawn_blocking(move || loop {
			if let Ok(packet) = capture.next_packet() {
				total_bytes_dev.fetch_add(packet.len().try_into().unwrap_or(0), Ordering::Relaxed);
			}
		});
	};
	Ok(())
}

fn open_capture_from_device(device_name: String) -> Result<Capture<Active>, pcap::Error> {
	let l_c = Capture::from_device(device_name.as_str())?
		.immediate_mode(true)
		.promisc(true)
		.open()?;
	Ok(l_c)
}
