use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use openide_remote::{
    auth::DeviceStore,
    config::{self, Config},
    host::Host,
    pairing::PairingStore,
    storage,
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(about = "Authenticated, durable mobile companion host for OpenIDE")]
struct Args {
    /// Private, separate host state. Never point this at desktop session data.
    #[arg(long)]
    state: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        /// Allow an explicitly configured Tailscale/private address. Use HTTPS
        /// or an encrypted private network; the built-in listener is HTTP.
        #[arg(long)]
        allow_remote_bind: bool,
    },
    DeviceAdd {
        name: String,
        /// The new bearer token is written once to this private JSON file.
        #[arg(long)]
        output: PathBuf,
    },
    DeviceList,
    /// Create a one-use invitation, valid for ten minutes. Secrets go only to private files.
    PairingStart {
        #[arg(long)]
        base_url: String,
        #[arg(long, default_value = "My PC")]
        label: String,
        #[arg(long)]
        output: PathBuf,
        /// Optional private SVG containing the pairing deep link.
        #[arg(long)]
        qr_output: Option<PathBuf>,
    },
    DeviceRevoke {
        id: String,
    },
    Check,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    storage::prepare_directory(&args.state)?;
    let devices = DeviceStore::new(&args.state);
    match args.command {
        Command::PairingStart {
            base_url,
            label,
            output,
            qr_output,
        } => {
            if output.exists()
                || qr_output
                    .as_ref()
                    .is_some_and(|path| path.exists() || path == &output)
            {
                bail!("pairing export already exists; choose new private files");
            }
            let store = PairingStore::new(&args.state);
            let invitation = store.start(&base_url, &label)?;
            let result = (|| -> Result<()> {
                storage::write_new_json(&output, &invitation)?;
                if let Some(path) = &qr_output {
                    write_new_private(path, invitation.qr_svg()?.as_bytes())?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                store.cancel(&invitation.pairing_id)?;
                return Err(error);
            }
            println!(
                "One-use invitation expires in 10 minutes. Private JSON: {}",
                output.display()
            );
            if let Some(path) = qr_output {
                println!("Private QR image: {}", path.display());
            }
        }
        Command::DeviceAdd { name, output } => {
            if output.exists() {
                bail!("pairing output already exists; choose a new private file");
            }
            let (device, token) = devices.issue(&name)?;
            storage::write_new_json(
                &output,
                &json!({"device_id":device.id,"name":device.name,"token":token}),
            )?;
            println!(
                "Device {} created. Pairing token written to {}",
                device.id,
                output.display()
            );
        }
        Command::DeviceList => {
            for device in devices.list()? {
                println!(
                    "{}\t{}\t{}",
                    device.id,
                    if device.revoked { "revoked" } else { "active" },
                    device.name
                );
            }
        }
        Command::DeviceRevoke { id } => {
            devices.revoke(&id)?;
            println!("Device revoked");
        }
        Command::Check => {
            Config::load(&args.state)?;
            println!("Configuration valid (credentials and network not tested)");
        }
        Command::Serve { allow_remote_bind } => {
            let config = Config::load(&args.state)?;
            if !config.bind.ip().is_loopback() && !allow_remote_bind {
                bail!(
                    "non-loopback listener requires --allow-remote-bind and HTTPS/private encrypted transport"
                );
            }
            let pairing = PairingStore::new(&args.state);
            if !devices.list()?.iter().any(|device| !device.revoked)
                && !pairing.has_active_invitation()?
            {
                bail!(
                    "create a pairing invitation with pairing-start or provision a device with device-add before serving"
                );
            }
            let model = config::build_backend(&config.provider)?;
            let host = Host::open(&args.state, config, model)?;
            let listener = tokio::net::TcpListener::bind(host.config.bind)
                .await
                .context("cannot bind host listener")?;
            println!(
                "OpenIDE Remote listening on {} (device authentication; pairing completion requires a one-use invitation)",
                listener.local_addr()?
            );
            let stopping = host.clone();
            let cleanup = tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    let store = pairing.clone();
                    let _ = tokio::task::spawn_blocking(move || store.purge_expired()).await;
                }
            });
            axum::serve(listener, openide_remote::api::router(host.clone()))
                .with_graceful_shutdown(async move {
                    shutdown_signal().await;
                    stopping.shutdown();
                })
                .await?;
            cleanup.abort();
            host.shutdown();
            for _ in 0..100 {
                if host.active_count() == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    Ok(())
}

fn write_new_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::{fs::OpenOptions, io::Write};
    let parent = path.parent().context("missing export directory")?;
    if !parent.exists() {
        storage::prepare_directory(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_argument_schema_is_valid() {
        Args::command().debug_assert();
        assert!(
            Args::try_parse_from(["openide-remote", "--state", "/tmp/private-test", "check"])
                .is_ok()
        );
        assert!(Args::try_parse_from(["openide-remote", "check"]).is_err());
    }
}
