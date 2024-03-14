use avail_light::{
	data::rocks_db::RocksDB,
	network::rpc,
	types::{ExponentialConfig, RetryConfig, State},
};
use clap::Parser;
use color_eyre::{eyre::Context, Result};
use kate_recovery::matrix::Position;
use std::sync::{Arc, Mutex};

#[derive(Parser)]
struct CommandArgs {
	#[arg(short, long, value_name = "URL", default_value_t = String::from("ws://localhost:9944"))]
	url: String,
	#[arg(short, long, value_name = "path", default_value_t = String::from("avail_path"))]
	avail_path: String,
}

#[tokio::main]
async fn main() -> Result<()> {
	let command_args = CommandArgs::parse();
	println!("Using URL: {}", command_args.url);
	println!("Using Path: {}", command_args.avail_path);
	let db = RocksDB::open(&command_args.avail_path)
		.wrap_err("API Compatibility Test could not initialize database")?;

	let state = Arc::new(Mutex::new(State::default()));
	let retry_cfg = RetryConfig::Exponential(ExponentialConfig {
		base: 10,
		max_delay: 4000,
		retries: 4,
	});

	let (rpc_client, _, subscriptions) =
		rpc::init(db, state, &[command_args.url], "DEV", retry_cfg).await?;
	tokio::spawn(subscriptions.run());

	let mut correct: bool = true;

	print!("Testing system version... ");
	let res = rpc_client.get_system_version().await;
	res_helper(&res, &mut correct);
	if let Ok(v) = res {
		println!("Reported system version: {v}")
	};

	print!("Testing runtime version... ");
	let res = rpc_client.get_runtime_version().await;
	res_helper(&res, &mut correct);
	if let Ok(v) = res {
		println!("Reported runtime version: {v:?}")
	};

	print!("Testing get head block header... ");
	let res = rpc_client.get_chain_head_header().await;
	let number = res.as_ref().unwrap().number; // TODO: Properly handle and skip if not working
	res_helper(&res, &mut correct);

	print!("Testing get head block hash... ");
	let res = rpc_client.get_finalized_head_hash().await;
	let hash = *res.as_ref().unwrap(); // TODO: Properly handle and skip if not working
	res_helper(&res, &mut correct);

	print!("Testing get block hash at height {number}... ");
	let res = rpc_client.get_block_hash(number).await;
	res_helper(&res, &mut correct);

	print!("Testing get header at height 1... ");
	let res = rpc_client.get_header_by_block_number(number).await;
	res_helper(&res, &mut correct);

	print!("Testing get header by hash... ");
	let res = rpc_client.get_header_by_hash(hash).await;
	res_helper(&res, &mut correct);

	print!("Testing get valset at height 1... ");
	let res = rpc_client.get_validator_set_by_block_number(number).await;
	res_helper(&res, &mut correct);

	print!("Testing get valset by hash... ");
	let res = rpc_client.get_validator_set_by_hash(hash).await;
	res_helper(&res, &mut correct);

	print!("Testing get set_id at height 1... ");
	let res = rpc_client.fetch_set_id_at(hash).await;
	res_helper(&res, &mut correct);

	print!("Testing get set_id by hash... ");
	let res = rpc_client.get_current_set_id_by_block_number(number).await;
	res_helper(&res, &mut correct);

	print!("Testing get_kate_proof for cell 0... ");
	let res = rpc_client
		.request_kate_proof(
			hash,
			&[Position {
				..Default::default()
			}],
		)
		.await;
	res_helper(&res, &mut correct);

	print!("Testing get_kate_row for row 0... ");
	let res = rpc_client.request_kate_rows(vec![0], hash).await;
	res_helper(&res, &mut correct);

	println!("Done");
	if !correct {
		std::process::exit(1);
	}
	Ok(())
}

fn res_helper<T, E>(res: &Result<T, E>, correct: &mut bool) {
	match res {
		Ok(_) => println!("✅"),
		_ => {
			*correct = false;
			println!("❌")
		},
	}
}
