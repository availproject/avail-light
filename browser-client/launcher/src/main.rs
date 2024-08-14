use std::{fs, path::PathBuf};

use actix_web::{http::header::ContentType, web, App, HttpResponse, HttpServer, Result};
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
	#[arg(short, long, default_value = "static")]
	static_dir: String,
	#[arg(short, long, default_value = "8080")]
	port: u16,
}

async fn index(data: web::Data<PathBuf>) -> Result<HttpResponse> {
	let index_path = data.join("index.html");
	let content = fs::read_to_string(index_path)
		.map_err(|_| actix_web::error::ErrorNotFound("index.html not found"))?;

	Ok(HttpResponse::Ok()
		.content_type(ContentType::html())
		.body(content))
}

async fn static_files(path: web::Path<String>, data: web::Data<PathBuf>) -> Result<HttpResponse> {
	let path = data.join(path.into_inner());

	if !path.exists() {
		return Err(actix_web::error::ErrorNotFound("File not found"));
	}

	let content = fs::read(&path)
		.map_err(|_| actix_web::error::ErrorInternalServerError("Failed to read file"))?;

	let content_type = mime_guess::from_path(&path).first_or_octet_stream();

	Ok(HttpResponse::Ok()
		.content_type(content_type.as_ref())
		.body(content))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
	let args = Args::parse();
	let static_dir = PathBuf::from(args.static_dir);

	if !static_dir.exists() || !static_dir.is_dir() {
		eprintln!("Error: The specified static directory does not exist or is not a directory.");
		std::process::exit(1);
	}

	info!("Starting server at http://localhost:8080");
	info!("Serving static files from: {}", static_dir.display());

	HttpServer::new(move || {
		App::new()
			.app_data(web::Data::new(static_dir.clone()))
			.service(web::resource("/").to(index))
			.service(web::resource("/index.html").to(index))
			.service(web::resource("/{filename:.*}").to(static_files))
	})
	.bind(("127.0.0.1", args.port))?
	.run()
	.await
}
