use anyhow::Result;
use avail_light::rpc_old;
use clap::Parser;
use kate_recovery::matrix::Position;

#[derive(Parser)]
struct CommandArgs {
	#[arg(short, long, value_name = "URL", default_value_t = String::from("ws://localhost:9944"))]
	url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
	let command_args = CommandArgs::parse();

	println!("Using URL: {}", command_args.url);
	let client = avail_subxt::build_client(command_args.url, false)
		.await
		.unwrap_or_else(|e| {
			eprintln!("Couldn't establish connection with node: {e}");
			std::process::exit(2);
		});

	let mut correct: bool = true;

	print!("Testing system version... ");
	let res = rpc_old::get_system_version(&client).await;
	res_helper(&res, &mut correct);
	if let Ok(v) = res {
		println!("Reported system version: {v}")
	};

	print!("Testing runtime version... ");
	let res = rpc_old::get_runtime_version(&client).await;
	res_helper(&res, &mut correct);
	if let Ok(v) = res {
		println!("Reported runtime version: {v:?}")
	};

	print!("Testing get head block header... ");
	let res = rpc_old::get_chain_head_header(&client).await;
	let number = res.as_ref().unwrap().number; // TODO: Properly handle and skip if not working
	res_helper(&res, &mut correct);

	print!("Testing get head block hash... ");
	let res = rpc_old::get_chain_head_hash(&client).await;
	let hash = *res.as_ref().unwrap(); // TODO: Properly handle and skip if not working
	res_helper(&res, &mut correct);

	print!("Testing get block hash at height {number}... ");
	let res = rpc_old::get_block_hash(&client, number).await;
	res_helper(&res, &mut correct);

	print!("Testing get header at height 1... ");
	let res = rpc_old::get_header_by_block_number(&client, number).await;
	res_helper(&res, &mut correct);

	print!("Testing get header by hash... ");
	let res = rpc_old::get_header_by_hash(&client, hash).await;
	res_helper(&res, &mut correct);

	print!("Testing get valset at height 1... ");
	let res = rpc_old::get_valset_by_block_number(&client, number).await;
	res_helper(&res, &mut correct);

	print!("Testing get valset by hash... ");
	let res = rpc_old::get_valset_by_hash(&client, hash).await;
	res_helper(&res, &mut correct);

	print!("Testing get set_id at height 1... ");
	let res = rpc_old::get_set_id_by_block_number(&client, number).await;
	res_helper(&res, &mut correct);

	print!("Testing get set_id by hash... ");
	let res = rpc_old::get_set_id_by_hash(&client, hash).await;
	res_helper(&res, &mut correct);

	print!("Testing get_kate_proof for cell 0... ");
	let res = rpc_old::get_kate_proof(
		&client,
		hash,
		&[Position {
			..Default::default()
		}],
	)
	.await;
	res_helper(&res, &mut correct);

	print!("Testing get_kate_row for row 0... ");
	let res = rpc_old::get_kate_rows(&client, vec![0], hash).await;
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
