use std::process::Command;

fn main() {
	let output = Command::new("git")
		.args(["rev-parse", "--short", "HEAD"])
		.output()
		.expect("Failed to get commit hash")
		.stdout;

	let commit_hash = String::from_utf8(output).expect("Invalid UTF-8 in command output");
	println!("cargo:rustc-env=GIT_COMMIT_HASH={}", commit_hash.trim());
}
