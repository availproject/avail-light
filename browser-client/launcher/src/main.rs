use actix_files as fs;
use actix_web::{web, App, HttpResponse, HttpServer, Result};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../wasm-client/static"]
struct StaticFiles;

async fn index() -> Result<HttpResponse> {
	let content = StaticFiles::get("index.html")
		.ok_or_else(|| actix_web::error::ErrorNotFound("index.html not found"))?;

	Ok(HttpResponse::Ok()
		.content_type("text/html; charset=utf-8")
		.body(content.data.into_owned()))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
	println!("Starting server at http://localhost:8080");

	HttpServer::new(|| {
		App::new()
			.service(web::resource("/").to(index))
			.service(web::resource("/index.html").to(index))
			.service(fs::Files::new("/", "./static").show_files_listing())
	})
	.bind(("127.0.0.1", 8080))?
	.run()
	.await
}
