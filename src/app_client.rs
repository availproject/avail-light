//! Application client for data fetching and reconstruction.
//!
//! App client is enabled when app_id is configured and greater than 0 in avail-light configuration. [`Light client`](super::light_client) triggers application client if block is verified with high enough confidence. Currently [`run`] function is separate task and doesn't block main thread.
//!
//! # Flow
//!
//! Get app data rows from node
//! Verify commitment equality for each row
//! Decode app data and store it into local database under the `app_id:block_number` key
//!
//! # Notes
//!
//! If application client fails to run or stops its execution, error is logged, and other tasks continue with execution.
use async_trait::async_trait;
use avail_core::AppId;
use avail_subxt::utils::H256;
use color_eyre::{
	eyre::{eyre, WrapErr},
	Result,
};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use kate_recovery::{
	com::{
		app_specific_rows, columns_positions, decode_app_extrinsics, reconstruct_columns, AppData,
		Percent,
	},
	commitments,
	config::{self, CHUNK_SIZE},
	data::{Cell, DataCell},
	matrix::{Dimensions, Position},
};
use mockall::automock;
use rand::SeedableRng as _;
use rand_chacha::ChaChaRng;
use std::{
	collections::{HashMap, HashSet},
	ops::Range,
	sync::{Arc, Mutex},
};
use tokio::sync::broadcast;
use tracing::{debug, error, info, instrument};

use crate::{
	data::{Database, Key},
	network::{p2p::Client as P2pClient, rpc::Client as RpcClient},
	proof,
	shutdown::Controller,
	types::{AppClientConfig, BlockVerified, OptionBlockRange, State},
};

#[async_trait]
#[automock]
trait Client {
	async fn reconstruct_rows_from_dht(
		&self,
		pp: Arc<PublicParameters>,
		block_number: u32,
		dimensions: Dimensions,
		commitments: &[[u8; config::COMMITMENT_SIZE]],
		missing_rows: &[u32],
	) -> Result<Vec<(u32, Vec<u8>)>>;

	async fn fetch_rows_from_dht(
		&self,
		block_number: u32,
		dimensions: Dimensions,
		row_indexes: &[u32],
	) -> Vec<Option<Vec<u8>>>;

	async fn get_kate_rows(
		&self,
		rows: Vec<u32>,
		dimensions: Dimensions,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>>;
}

#[derive(Clone)]
struct AppClient {
	p2p_client: P2pClient,
	rpc_client: RpcClient,
}

#[async_trait]
impl Client for AppClient {
	async fn reconstruct_rows_from_dht(
		&self,
		pp: Arc<PublicParameters>,
		block_number: u32,
		dimensions: Dimensions,
		commitments: &[[u8; config::COMMITMENT_SIZE]],
		missing_rows: &[u32],
	) -> Result<Vec<(u32, Vec<u8>)>> {
		let missing_cells = dimensions.extended_rows_positions(missing_rows);

		if missing_cells.is_empty() {
			return Ok(vec![]);
		}

		debug!(
			block_number,
			"Fetching {} missing row cells from DHT",
			missing_cells.len()
		);
		let (fetched, unfetched) = fetch_verified(
			pp.clone(),
			&self.p2p_client,
			block_number,
			dimensions,
			commitments,
			&missing_cells,
		)
		.await?;
		debug!(
			block_number,
			"Fetched {} row cells, {} row cells is missing",
			fetched.len(),
			unfetched.len()
		);

		let mut rng = ChaChaRng::from_seed(Default::default());
		let missing_cells =
			columns_positions(dimensions, &unfetched, Percent::from_percent(66), &mut rng);

		let (missing_fetched, _) = fetch_verified(
			pp,
			&self.p2p_client,
			block_number,
			dimensions,
			commitments,
			&missing_cells,
		)
		.await?;

		let reconstructed = reconstruct_columns(dimensions, &missing_fetched)?;

		debug!(
			block_number,
			"Reconstructed {} columns: {:?}",
			reconstructed.keys().len(),
			reconstructed.keys()
		);

		let mut reconstructed_cells = unfetched
			.into_iter()
			.map(|position| data_cell(position, &reconstructed))
			.collect::<Result<Vec<_>>>()?;

		debug!(
			block_number,
			"Reconstructed {} missing row cells",
			reconstructed_cells.len()
		);

		let mut data_cells: Vec<DataCell> = fetched.into_iter().map(Into::into).collect::<Vec<_>>();

		data_cells.append(&mut reconstructed_cells);

		data_cells.sort_by(|a, b| {
			(a.position.row, a.position.col).cmp(&(b.position.row, b.position.col))
		});

		missing_rows
			.iter()
			.map(|&row| {
				let data = data_cells
					.iter()
					.filter(|&cell| cell.position.row == row)
					.flat_map(|cell| cell.data)
					.collect::<Vec<_>>();

				if data.len() != dimensions.width() * config::CHUNK_SIZE {
					return Err(eyre!("Row size is not valid after reconstruction"));
				}

				Ok((row, data))
			})
			.collect::<Result<Vec<_>>>()
	}

	async fn fetch_rows_from_dht(
		&self,
		block_number: u32,
		dimensions: Dimensions,
		row_indexes: &[u32],
	) -> Vec<Option<Vec<u8>>> {
		self.p2p_client
			.fetch_rows_from_dht(block_number, dimensions, row_indexes)
			.await
	}

	async fn get_kate_rows(
		&self,
		rows: Vec<u32>,
		dimensions: Dimensions,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>> {
		let rows = rows
			.clone()
			.into_iter()
			.zip(self.rpc_client.request_kate_rows(rows, block_hash).await?);
		let mut result = vec![None; dimensions.extended_rows() as usize];
		for (i, row) in rows {
			result[i as usize] = row;
		}
		Ok(result)
	}
}

fn new_data_cell(row: usize, col: usize, data: &[u8]) -> Result<DataCell> {
	Ok(DataCell {
		position: Position {
			row: row.try_into()?,
			col: col.try_into()?,
		},
		data: data.try_into()?,
	})
}

fn data_cells_from_row(row: usize, row_data: &[u8]) -> Result<Vec<DataCell>> {
	row_data
		.chunks_exact(CHUNK_SIZE)
		.enumerate()
		.map(move |(col, data)| new_data_cell(row, col, data))
		.collect::<Result<Vec<DataCell>>>()
}

fn data_cells_from_rows(rows: Vec<Option<Vec<u8>>>) -> Result<Vec<DataCell>> {
	Ok(rows
		.into_iter()
		.enumerate() // Add row indexes
		.filter_map(|(row, row_data)| row_data.map(|data| (row, data))) // Remove None rows
		.map(|(row, data)| data_cells_from_row(row, &data))
		.collect::<Result<Vec<Vec<DataCell>>, _>>()?
		.into_iter()
		.flatten()
		.collect::<Vec<_>>())
}

fn data_cell(
	position: Position,
	reconstructed: &HashMap<u16, Vec<[u8; config::CHUNK_SIZE]>>,
) -> Result<DataCell> {
	let row: usize = position.row.try_into()?;
	reconstructed
		.get(&position.col)
		// Dividing with extension factor since reconstructed column is not extended
		.and_then(|column| column.get(row / config::EXTENSION_FACTOR))
		.map(|&data| DataCell { position, data })
		.ok_or_else(|| eyre!("Data cell not found"))
}

async fn fetch_verified(
	pp: Arc<PublicParameters>,
	p2p_client: &P2pClient,
	block_number: u32,
	dimensions: Dimensions,
	commitments: &[[u8; config::COMMITMENT_SIZE]],
	positions: &[Position],
) -> Result<(Vec<Cell>, Vec<Position>)> {
	let (mut fetched, mut unfetched) = p2p_client
		.fetch_cells_from_dht(block_number, positions)
		.await;

	let (verified, mut unverified) =
		proof::verify(block_number, dimensions, &fetched, commitments, pp)
			.await
			.wrap_err("Failed to verify fetched cells")?;

	fetched.retain(|cell| verified.contains(&cell.position));
	unfetched.append(&mut unverified);

	Ok((fetched, unfetched))
}

#[instrument(skip_all, fields(block = block.block_num), level = "trace")]
async fn process_block(
	client: impl Client,
	db: impl Database,
	cfg: &AppClientConfig,
	app_id: AppId,
	block: &BlockVerified,
	pp: Arc<PublicParameters>,
) -> Result<AppData> {
	let lookup = &block.lookup;
	let block_number = block.block_num;
	let dimensions = block.dimensions;

	let commitments = &block.commitments;

	let app_rows = app_specific_rows(lookup, dimensions, app_id);

	debug!(
		block_number,
		"Fetching {} app rows from DHT: {app_rows:?}",
		app_rows.len()
	);

	let dht_rows = client
		.fetch_rows_from_dht(block_number, dimensions, &app_rows)
		.await;

	let dht_rows_count = dht_rows.iter().flatten().count();
	debug!(block_number, "Fetched {dht_rows_count} app rows from DHT");

	let (dht_verified_rows, dht_missing_rows) =
		commitments::verify_equality(&pp, commitments, &dht_rows, lookup, dimensions, app_id)?;
	debug!(
		block_number,
		"Verified {} app rows from DHT, missing {}",
		dht_verified_rows.len(),
		dht_missing_rows.len()
	);

	let rpc_rows = if cfg.disable_rpc {
		vec![None; dht_rows.len()]
	} else {
		debug!(
			block_number,
			"Fetching missing app rows from RPC: {dht_missing_rows:?}",
		);
		client
			.get_kate_rows(dht_missing_rows, dimensions, block.header_hash)
			.await?
	};

	let (rpc_verified_rows, mut missing_rows) =
		commitments::verify_equality(&pp, commitments, &rpc_rows, lookup, dimensions, app_id)?;
	// Since verify_equality returns all missing rows, exclude DHT rows that are already verified
	missing_rows.retain(|row| !dht_verified_rows.contains(row));

	debug!(
		block_number,
		"Verified {} app rows from RPC, missing {}",
		rpc_verified_rows.len(),
		missing_rows.len()
	);

	let verified_rows_iter = dht_verified_rows
		.into_iter()
		.chain(rpc_verified_rows.into_iter());
	let verified_rows: HashSet<u32> = HashSet::from_iter(verified_rows_iter);

	let mut rows = dht_rows
		.into_iter()
		.zip(rpc_rows.into_iter())
		.zip(0..dimensions.extended_rows())
		.map(|((dht_row, rpc_row), row_index)| {
			let row = dht_row.or(rpc_row)?;
			verified_rows.contains(&row_index).then_some(row)
		})
		.collect::<Vec<_>>();

	let rows_count = rows.iter().filter(|row| row.is_some()).count();
	debug!(
		block_number,
		"Found {rows_count} rows, verified {}, {} is missing",
		verified_rows.len(),
		missing_rows.len()
	);

	if missing_rows.len() * dimensions.width() > cfg.threshold {
		return Err(eyre!("Too many cells are missing"));
	}

	debug!(
		block_number,
		"Reconstructing {} missing app rows from DHT: {missing_rows:?}",
		missing_rows.len()
	);

	let dht_rows = client
		.reconstruct_rows_from_dht(pp, block_number, dimensions, commitments, &missing_rows)
		.await?;

	debug!(
		block_number,
		"Reconstructed {} app rows from DHT",
		dht_rows.len()
	);

	for (row_index, row) in dht_rows {
		let i: usize = row_index.try_into()?;
		rows[i] = Some(row);
	}

	let data_cells = data_cells_from_rows(rows)
		.wrap_err("Failed to create data cells from rows got from RPC")?;

	let data = decode_app_extrinsics(lookup, dimensions, data_cells, app_id)
		.wrap_err("Failed to decode app extrinsics")?;

	debug!(block_number, "Storing data into database");

	// store encoded App Data into the database
	db.put(Key::AppData(app_id.0, block_number), data.clone())
		.wrap_err("App Client failed to store App Data into database")?;

	let bytes_count = data.iter().fold(0usize, |acc, x| acc + x.len());
	debug!(block_number, "Stored {bytes_count} bytes into database");

	Ok(data)
}

/// Runs application client.
///
/// # Arguments
///
/// * `cfg` - Application client configuration
/// * `db` - Database to store data inot DB
/// * `network_client` - Reference to a libp2p custom network client
/// * `rpc_client` - Node's RPC subxt client for fetching data unavailable in DHT (if configured)
/// * `app_id` - Application ID
/// * `block_receive` - Channel used to receive header of verified block
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
#[allow(clippy::too_many_arguments)]
pub async fn run(
	cfg: AppClientConfig,
	db: impl Database + Clone + Sync,
	network_client: P2pClient,
	rpc_client: RpcClient,
	app_id: AppId,
	mut block_receive: broadcast::Receiver<BlockVerified>,
	pp: Arc<PublicParameters>,
	state: Arc<Mutex<State>>,
	sync_range: Range<u32>,
	data_verified_sender: broadcast::Sender<(u32, AppData)>,
	shutdown: Controller<String>,
) {
	info!("Starting for app {app_id}...");

	fn set_data_verified_state(
		state: Arc<Mutex<State>>,
		sync_range: &Range<u32>,
		block_number: u32,
	) {
		let mut state = state.lock().expect("State lock can be acquired");
		match sync_range.contains(&block_number) {
			true => state.sync_data_verified.set(block_number),
			false => state.data_verified.set(block_number),
		}
		if state.synced == Some(false) && sync_range.clone().last() == Some(block_number) {
			state.synced.replace(true);
		};
	}

	loop {
		let block = match block_receive.recv().await {
			Ok(block) => block,
			Err(error) => {
				error!("Cannot receive message: {error}");
				let _ = shutdown.trigger_shutdown(format!("Cannot receive message: {error:#}"));
				return;
			},
		};

		let block_number = block.block_num;
		let dimensions = &block.dimensions;

		info!(block_number, "Block available: {dimensions:?}");

		if block.lookup.range_of(app_id).is_none() {
			info!(
				block_number,
				"Skipping block with no cells for app {app_id}"
			);
			set_data_verified_state(state.clone(), &sync_range, block_number);
			continue;
		}

		let app_client = AppClient {
			p2p_client: network_client.clone(),
			rpc_client: rpc_client.clone(),
		};
		let data =
			match process_block(app_client, db.clone(), &cfg, app_id, &block, pp.clone()).await {
				Ok(data) => data,
				Err(error) => {
					error!(block_number, "Cannot process block: {error}");
					let _ = shutdown.trigger_shutdown(format!("Cannot process block: {error:#}"));
					return;
				},
			};
		set_data_verified_state(state.clone(), &sync_range, block_number);
		if let Err(error) = data_verified_sender.send((block_number, data)) {
			error!("Cannot send data verified message: {error}");
			let _ =
				shutdown.trigger_shutdown(format!("Cannot send data verified message: {error:#}"));
			return;
		}
		debug!(block_number, "Block processed");
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		data::mem_db,
		types::{AppClientConfig, RuntimeConfig},
	};
	use avail_core::DataLookup;
	use hex_literal::hex;
	use kate_recovery::{matrix::Dimensions, testnet};

	#[tokio::test]
	async fn test_process_blocks_without_rpc() {
		let mut cfg = AppClientConfig::from(&RuntimeConfig::default());
		cfg.disable_rpc = true;
		let pp = Arc::new(testnet::public_params(1024));
		let dimensions: Dimensions = Dimensions::new(1, 128).unwrap();
		let mut mock_client = MockClient::new();
		let db = mem_db::MemoryDB::default();
		let dht_fetched_rows: Vec<Option<Vec<u8>>> = [
			Some(hex!("042c280403000be11c4de187018000000000000000000000000000000000000004f920f1208400d43593c715fdd31c61141abd04a99fd6822c8558854ccde3009a5684e7a56da27d01849d181eab318cbda6d664fda766d160ff2a70b5f00100a12608c9a404eb7afe393412a4860e705e7cb9a23747f6198505a95bd1bdb000c327962e4c8c0471567488b4001400041d01411f32373532323063316239360063353331636464386237313265633263326632353038306631393164623033003965346162353663623838623334356165333533613565626566616361363300393438323736366135666333386430323932316637303432323434366432610034396239343937646232366438336339653261656236316538373033373933003762326238353133323662343739366565663833636338653732363335623200666564393261306665653162393564656164326665623134333164343634330062636331353865353639653039643333303565663564376461386231396463003635633237663962343763303231663161373031363664303932313663636500393334343561653036623964343532333031356266323934356232316361660061386338396464303437386438326233633030373331613732363531633534003736396438616231303461623735656162613164653766376536376264633800396532353631313565303964353237663231643838396262633231626363630031343766383566626338366366663036383333343934393531393064656561006131656331333232343531363034313563626631643339393838666436656600393164323939396636353164383066353062343766313630363936623737660038366261326463326233396339653066343262383336353030313964373261006430376461643462646132343130333332376130313863313131326665343500613537386366393332373631623232653936316533633262633439323561610036303265633533393036646635393266636334633331393536346436646137003333363462313636353265346337336664306230303034323661393865663700633062636634303638333838383134393038376462313162653361303430360038363733336634313937653236303138376466303434336538373730383933003966616336343936333864363736336365373962346161643430653936616400646530306166633761363565653632646536336231646230623833666161610037646566303837303061616539346261313538393935383239313934376161003366356666373539643862633631363066366433646163613139363266346100333134353761303664363135616130636334626463663732666436623330640038333034316234326165373662393761366231306465623134623263636637003837633238623436346362316232316664306139326434353833343861396300396535393663353564636535383566366666376238383232616439616366320039393033323731356134366165666337623962616661386566636461626634006661633634313731396464343665616235623234663439346536623433393100306666363335353532333537623265356434356534666637343438393462380038386661386465393632396630333435323064343131613362626239303635003633336536663731363932656230653561656136653535643165396138323600383161323963323338623932393665356639633837626630613131306563320035343035353733383164653562663930393831313238623031366132303438006639383030333336366330356230353839656539643363643331326135386200613932613532373939363564303839633965366134303231363866363265390061623432613530386464376166353865396634643633633930373031633130003837366636363633656265326166613463633337363164316532383737363700363866303330636638353162366466376230356263653334633464383633350062613637366436343463313339616531373939353062646162633064663739006336666336643936343336363237363637353566653335363935633438313000383331646431373537396563623465383039343633333332333832353331610038353365346231653239376463363535613931343330626234353834313135003336373166316364616366373931326233613232306363633632353436323600393734323939616232373038666165363234313030656639653037636533310066643236336536303737336562373963353137353130623833373765343363003762303438356330316437366432313961326132346133616565616434663300636537393931303430373861373935393164663030613738343738626234330039666461613036333337626432656130343063636435343539666136666662003837383037643035373266666131373261656230383936343435353763376600303365326163626331626466633333326163663365656331393762356466330065653132383232336565656165333962633035343137363030393835343030003338343435653236383430616566316137373664613963343864626634383000323838326663303735383638633232613837383937666334383065333766360039633963666166313435326461343939346562663564353464336634656361003765383466326635633064343566653635663237636462633963313538383700306231316463303132653632353962643937373037613461393663613063330063366438356366343839336566363232313065616334343737313330633263003664393130346662633561373261623865346534373163363666623831663800656563656337353861653665393163386364666562656165373730663339350034643964306236343238363338373334383161666235626336623938383333003739663863800000000000000000000000000000000000000000000000000000346080be83f48ad1748c4ad339abdcb803368efdd1f65689619ff8c208755d0084eefcf837b61c479b3332059bc8e89b490a9d502baecaed448433d4e161710000a71cbb1a0387598e509d9fcab511022f437b0caf13591315c3f1bbf04f18009d83f014806210da6ee1d2f80cf0f9c08f1d132be042769015f6174fd2b24c00fa1f4fd88a21072597d1ec63ad485bd4d53a146337832ef54acf4f602dad410092102a82cf823de7404c7f498a50265c6eae2476e635712fcca199ed31bf56006a93f0fbaef7d3b743b6910c9719b540f78ef87ff34b33cf0e5fbb40b83f4400aee2a1f1f39cb1246345b3deaa5b48049e372d6f63d68d1821010de93ae42d000233cfbffd5777a869859b435a07832f0dc09377e8531b0ae487e11e960ff700236ca1244d8636a13030f5a468b159da05a8f3777ea87a6bdefe58dc9be0850096229c83f39c0a4c54d84b87f720a7f6374ff379eae98e5dc273b36c72135300d6aa0ac8a852aca990b8bda2c224c23a6ef9fc64d3d78204a45bff2d77a69d007ec8a1671edea11591792506dc6f24598883002c5105f02561d5053bc3ee10003749467c9d7e121539399cb5b9a94cd6fd91095280d7127f87293309a4a89500137f2a655e98781a226f12b61e1c16d85500c8f6f23b3570122438d802f5ec00ba964cf7e0b0b35b6bac4d3f15bb8deec58b55c41eda9afdbc2694b5d6ffeb0041882be8f7c52cbaccc30098dc880d370602d917239ccb667fac94f37461f400e555f26a0b21907f6ebf869e88638c8c7f24b85c71568495e94593abec16ec00c4127f1e9a1fd9b876f1462f868d9da4d2ef0c6fb26d482d0114ff426514ad0049b6f773d05d3b6cf01dae70e2c0ac38c9448edda4907577b7a5c655099427006a05a737a1c74b57ae652ce3a28687f32c64eddf6e28a7f58b3bf30684f5740075a0e3824345cca425df01ba10ce381bcf5192ff90b4d4f3355a3faee3ffbf00af5d9bfccc77f3216fd6153e348dda995add52361fe302536b06ee5018786c0034c1a58f5e55f17353ce05f8b53c6a40742bc2baffc370b850479c64fdb6b20078a36c5c98efed26b8f3be59420d4b69b442233a334ba5cac80e65459818ee00b286a8a6880b3431eef2b1a2257eaf3c004db48ae089319a0d0a28f5e41b57004e539fd4000c7719937aff31c4f728fefff2a095b183134bc1c0f6bfc44b2200377a7afeec903b4f6813cb57709410b678bd3746fb33f80516e6c27d441ed0009e1dd3c7ab08abc1857d74c290a538b7897a4d207250d45215fdbe042f296f00cff960d4099f3c379c16caba34ac9dacf38bdbf2406ced2e004679a51cdcc20061ef41f450fe7528f1551ab47c34bd3e5bf58a004bd916f8cf9da3be668297004c2c64c1556131706c52c7ee6b2785b4c3dc33923f9839eb1c2e86a535875a00dec8c87b4c504ae5994957029bb940223da75860fc992e9dc7d88fd027d4a6008327c7915f4c84b38f48171a240a3db3e4ceeef905904ac6439c1a5432d4f200f948c1c1d242902c068b042c558334c07f24148c11d1ccdc27c52ecbd07c850052c8a1ed02f44e535027be3163600f130a0467a4936447d26708129c2bbf5800d626a197562a974a02b2f638d480126a82ab24239329628c5958e912fbff940019c821dbaced432ebb5ea363cb83ba9e46d98ba97fd5669b6119d44635182a0051e7aee66fc4d118024509b0441a9d030f1496e8d3c9b62a416c5fab6e09340062552c7d472e8bc87bdc0b0035382f13d5f51d896f06a0d046db4b47e91d0a00e6f1de369548d8d1607d7ede6af7dbe5c6af21cf4ca1638509b07f77ec9224006b0578e0a8108d500e4d3bbf5e8cecc7f81b9646fcd74fb6fa4010597210de00bc1bd58579ca67a403cd79d4b42d7d3019f6528cc19e13a97aa6bd15c5df840036091884dd7769d2fbe0f44d6f6a1104afaf9a449fffdd31bade8d0747fce800083f4629573dc80e2e427dd3c0072938ba5d37b557af5fcc59b3970b02d93f00fc34ac24a28bd27ddc9a08708e687b3e231961a0ac0eb57299e1bf604fb54d00c3b57593dda7ded42dc3c33a347ef257923b8268c6c8e366ab92b9566860ea00d3aff127b154bf457ea86505972c23d10c5ccbd0112ae8ea23ad943ef50cae005082c3be2c912f8267f34d94a18b741109de2f10380a1c225da70ecbea599f0015be0db3894fa6e39db4665edf8ec16045733326c1646567ee3d8c20f4cb19004e9e7ed617349fb347dcf87794e0afa2293e2b5dda1e582b492543d42e40e800427fdadd2cf2787390df636c786fab37d014e5d2a5fd60bca54d300a3f8ef80078574b460999d4956f31ec0f01fbb155437b500659c5630eb68b75d9c85b8d0020d391d6e6e508663a2137b6474487a82b4fd0b1166896847aa214a2e864ab00ff4d6f74633099490b30722650af03f840e15ab51b27d0ed7fc8ab5bdc04ef00ec07911905384ea4056dc703c8e7ec15a2d2712528a65cdd6331b5719c2f23009e2d946819185731727828a52c4aede37c4e6dba05c8f050dadebe10fdd115005ea772df40e4cfaf532fe810e8e87c6e33040a51e328202e037c0ccbea6e8100").to_vec()),
			None,
		]
		.to_vec();

		let id_lens: Vec<(u32, usize)> = vec![(0, 1), (1, 69)];
		let lookup = DataLookup::from_id_and_len_iter(id_lens.into_iter()).unwrap();
		let block = BlockVerified {
			header_hash: hex!("ec30fcc1f32db0f51ce6305c2601089741ea0b42853f402194b49b04bf936338")
				.into(),
			block_num: 270,
			dimensions,
			lookup,
			commitments: [
				[
					171, 159, 250, 70, 135, 100, 125, 155, 243, 109, 243, 101, 158, 165, 185, 147,
					73, 58, 163, 138, 88, 81, 86, 159, 103, 164, 86, 46, 208, 19, 86, 30, 10, 59,
					45, 229, 54, 119, 29, 226, 134, 68, 186, 219, 36, 20, 181, 99,
				],
				[
					171, 159, 250, 70, 135, 100, 125, 155, 243, 109, 243, 101, 158, 165, 185, 147,
					73, 58, 163, 138, 88, 81, 86, 159, 103, 164, 86, 46, 208, 19, 86, 30, 10, 59,
					45, 229, 54, 119, 29, 226, 134, 68, 186, 219, 36, 20, 181, 99,
				],
			]
			.to_vec(),
			confidence: None,
		};
		mock_client
			.expect_fetch_rows_from_dht()
			.returning(move |_, _, _| {
				let dht_rows_clone = dht_fetched_rows.clone();
				Box::pin(async move { dht_rows_clone })
			});
		if cfg.disable_rpc {
			mock_client.expect_get_kate_rows().never();
		}
		mock_client
			.expect_reconstruct_rows_from_dht()
			.returning(|_, _, _, _, _| Box::pin(async move { Ok(vec![]) }));

		process_block(mock_client, db, &cfg, AppId(1), &block, pp)
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_process_block_with_rpc() {
		let cfg = AppClientConfig::from(&RuntimeConfig::default());
		let pp = Arc::new(testnet::public_params(1024));
		let dimensions: Dimensions = Dimensions::new(1, 16).unwrap();
		let mut mock_client = MockClient::new();
		let db = mem_db::MemoryDB::default();
		let dht_rows: Vec<Option<Vec<u8>>> = vec![None, None];
		let kate_rows: Vec<Option<Vec<u8>>> = [
			Some(hex!("042c280403000ba3fa0ab887018000000000000000000000000000000000000004d904d1048400d43593c715fdd31c61141abd04a99fd6822c8558854ccde3009a5684e7a56da27d01a8cf58e1e9c735f93ebc7a94086aa27cfd77db173aac00803895886b8a4f49e85c68f469d570f0ed992750bf95329bb90ef56b45abcd009fedef0d9cbdd61c05a181d4013800041d0121033036343265356430346236003632353966363635666431353361613136646637343066323533373237386600613139316565393630343862663839393733343961303137353865346237610032643539663534353338393865626231643233626634353965363637613633003462313663663432326663393335336434623862623630386235393230653400353733663335663037303764333238616661343832316663656631363439660039643532653762353732356533303935643865656561356436633235333830006434658000000000000000000000000000000000000000000000000000000000346080be83f48ad1748c4ad339abdcb803368efdd1f65689619ff8c208755d0084eefcf837b61c479b3332059bc8e89b490a9d502baecaed448433d4e161710000a71cbb1a0387598e509d9fcab511022f437b0caf13591315c3f1bbf04f18009d83f014806210da6ee1d2f80cf0f9c08f1d132be042769015f6174fd2b24c00").to_vec()),
			None,
		]
		.to_vec();

		let id_lens: Vec<(u32, usize)> = vec![(0, 1), (1, 11)];
		let lookup = DataLookup::from_id_and_len_iter(id_lens.into_iter()).unwrap();
		let block = BlockVerified {
			header_hash: hex!("5bc959e1d05c68f7e1b5bc3a83cfba4efe636ce7f86102c30bcd6a2794e75afe")
				.into(),
			block_num: 288,
			dimensions,
			lookup,
			commitments: [
				[
					165, 227, 207, 130, 59, 77, 78, 242, 184, 232, 114, 218, 145, 167, 149, 53, 89,
					7, 230, 49, 85, 113, 218, 116, 43, 195, 144, 203, 149, 114, 106, 89, 73, 164,
					17, 163, 3, 145, 173, 6, 119, 222, 17, 60, 251, 215, 40, 192,
				],
				[
					165, 227, 207, 130, 59, 77, 78, 242, 184, 232, 114, 218, 145, 167, 149, 53, 89,
					7, 230, 49, 85, 113, 218, 116, 43, 195, 144, 203, 149, 114, 106, 89, 73, 164,
					17, 163, 3, 145, 173, 6, 119, 222, 17, 60, 251, 215, 40, 192,
				],
			]
			.to_vec(),
			confidence: None,
		};
		mock_client
			.expect_fetch_rows_from_dht()
			.returning(move |_, _, _| {
				let dht_rows_clone = dht_rows.clone();
				Box::pin(async move { dht_rows_clone })
			});
		if cfg.disable_rpc {
			mock_client.expect_get_kate_rows().never();
		} else {
			mock_client
				.expect_get_kate_rows()
				.returning(move |_, _, _| {
					let kate_rows_clone = kate_rows.clone();
					Box::pin(async move { Ok(kate_rows_clone) })
				});
		}
		mock_client
			.expect_reconstruct_rows_from_dht()
			.returning(|_, _, _, _, _| Box::pin(async move { Ok(vec![]) }));

		process_block(mock_client, db, &cfg, AppId(1), &block, pp)
			.await
			.unwrap();
	}
}
