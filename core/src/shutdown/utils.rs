#[cfg(not(target_arch = "wasm32"))]
/// This utility function returns a [`Future`] that completes upon
/// receiving each of the default termination signals.
///
/// On Unix-based systems, these signals are Ctrl-C (SIGINT) or SIGTERM,
/// and on Windows, they are Ctrl-C, Ctrl-Close, Ctrl-Shutdown.
pub async fn user_signal() {
	let ctrl_c = tokio::signal::ctrl_c();
	#[cfg(all(unix, not(windows)))]
	{
		let sig = async {
			let mut os_sig =
				tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
			os_sig.recv().await;
			std::io::Result::Ok(())
		};

		tokio::select! {
			_ = ctrl_c => {},
			_ = sig => {}
		}
	}

	#[cfg(all(not(unix), windows))]
	{
		let ctrl_close = async {
			let mut sig = tokio::signal::windows::ctrl_close()?;
			sig.recv().await;
			std::io::Result::Ok(())
		};
		let ctrl_shutdown = async {
			let mut sig = tokio::signal::windows::ctrl_shutdown()?;
			sig.recv().await;
			std::io::Result::Ok(())
		};
		tokio::select! {
			_ = ctrl_c => {},
			_ = ctrl_close => {},
			_ = ctrl_shutdown => {},
		}
	}
}
