use actix_web::{http::header::ContentType, web, App, HttpResponse, HttpServer, Result};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../wasm-client/static"]
struct StaticFiles;

async fn index() -> Result<HttpResponse> {
	let content = StaticFiles::get("index.html")
		.ok_or_else(|| actix_web::error::ErrorNotFound("index.html not found"))?;

	Ok(HttpResponse::Ok()
		.content_type(ContentType::html())
		.body(content.data.into_owned()))
}

async fn static_files(path: web::Path<String>) -> Result<HttpResponse> {
	let path = path.into_inner();
	let content = StaticFiles::get(&path)
		.ok_or_else(|| actix_web::error::ErrorNotFound(format!("File not found: {}", path)))?;

	let content_type = mime_guess::from_path(&path).first_or_octet_stream();

	Ok(HttpResponse::Ok()
		.content_type(content_type.as_ref())
		.body(content.data.into_owned()))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
	println!("Starting server at http://localhost:8080");

	HttpServer::new(|| {
		App::new()
			.service(web::resource("/").to(index))
			.service(web::resource("/index.html").to(index))
			.service(web::resource("/{filename:.*}").to(static_files))
	})
	.bind(("127.0.0.1", 8080))?
	.run()
	.await
}
