// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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
        let database =
            substrate_lite::database::sled::open(substrate_lite::database::sled::Config {
                path: &db_path.join("polkadot"),
            })
            .unwrap();

        match database {
            substrate_lite::database::sled::DatabaseOpen::Open(db) => db,
            substrate_lite::database::sled::DatabaseOpen::Empty(_) => {
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
