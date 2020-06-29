//! Small example of a secondary program. Examines the database.

fn main() {
    futures::executor::block_on(async_main())
}

async fn async_main() {
    const APP_INFO: app_dirs::AppInfo = app_dirs::AppInfo {
        name: "substrate-lite",
        author: "tomaka17",
    };

    let database = {
        let db_path =
            app_dirs::app_dir(app_dirs::AppDataType::UserData, &APP_INFO, "database").unwrap();
        let database = substrate_lite::database::open(substrate_lite::database::Config {
            path: &db_path.join("polkadot"),
        })
        .unwrap();

        match database {
            substrate_lite::database::DatabaseOpen::Open(db) => db,
            substrate_lite::database::DatabaseOpen::Empty(_) => {
                println!("No database yet.");
                return;
            }
        }
    };

    println!(
        "Database best block: #{}",
        database.best_block_number().unwrap()
    );
}
