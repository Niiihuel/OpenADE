//! The home router's port, opened when the person asks for it (UPnP), so a
//! phone away from home reaches the PC over IPv4.

use igd_next::aio::tokio::search_gateway;
use igd_next::{PortMappingProtocol, SearchOptions};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the router keeps the port open unless renewed.
const LEASE_SECONDS: u32 = 3600;
const RENEW: Duration = Duration::from_secs(1800);
const DESCRIPTION: &str = "openADE Remote";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpnpState {
    Off,
    Searching,
    /// Open, reached at this public address.
    Open(Ipv4Addr),
    /// Open, but the router's own address is not public: the provider
    /// shares one address among many homes (CGNAT), or there is a second router.
    Shared(Ipv4Addr),
    Failed(String),
}

impl UpnpState {
    pub fn describe(&self) -> String {
        match self {
            Self::Off => "Apagado".into(),
            Self::Searching => "Buscando el router…".into(),
            Self::Open(ip) => format!("Abierto: desde afuera se llega por {ip}"),
            Self::Shared(ip) => format!(
                "El router abrió el puerto, pero su dirección ({ip}) no es pública: tu proveedor comparte la conexión. Desde afuera solo sirve IPv6 o una dirección pública."
            ),
            Self::Failed(reason) => reason.clone(),
        }
    }

    pub(crate) fn route(&self, port: u16) -> Option<String> {
        match self {
            Self::Open(ip) => Some(format!("https://{ip}:{port}")),
            _ => None,
        }
    }
}

/// An address the internet cannot reach: private, shared (100.64/10) or local.
fn shared(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    ip.is_private() || ip.is_loopback() || ip.is_link_local() || (first == 100 && second & 0xc0 == 64)
}

fn options() -> SearchOptions {
    let mut options = SearchOptions::default();
    options.timeout = Some(Duration::from_secs(5));
    options
}

async fn open(port: u16) -> Result<Ipv4Addr, String> {
    let gateway = search_gateway(options()).await.map_err(|_| "No se encontró un router con UPnP en esta red".to_owned())?;
    let local = crate::network::lan_ipv4().ok_or("El PC no tiene dirección en la red local")?;
    gateway
        .add_port(PortMappingProtocol::TCP, port, SocketAddr::new(local.into(), port), LEASE_SECONDS, DESCRIPTION)
        .await
        .map_err(|error| format!("El router no quiso abrir el puerto {port}: {error}"))?;
    match gateway.get_external_ip().await {
        Ok(IpAddr::V4(ip)) => Ok(ip),
        _ => Err("El router abrió el puerto pero no dijo su dirección pública".into()),
    }
}

/// Keeps the port open until `stop` fires, then closes it.
pub(crate) async fn keep_open(port: u16, state: Arc<Mutex<UpnpState>>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let set = |value: UpnpState| *state.lock().unwrap_or_else(|error| error.into_inner()) = value;
    set(UpnpState::Searching);
    loop {
        set(match open(port).await {
            Ok(ip) if shared(ip) => UpnpState::Shared(ip),
            Ok(ip) => UpnpState::Open(ip),
            Err(reason) => UpnpState::Failed(reason),
        });
        tokio::select! {
            _ = tokio::time::sleep(RENEW) => {}
            _ = stop.changed() => break,
        }
    }
    let close = async {
        if let Ok(gateway) = search_gateway(options()).await {
            let _ = gateway.remove_port(PortMappingProtocol::TCP, port).await;
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(6), close).await;
    set(UpnpState::Off);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_router_behind_another_or_a_shared_provider_address_is_not_a_route() {
        for ip in ["192.168.0.10", "10.1.2.3", "100.72.0.9", "172.20.0.1"] {
            assert!(shared(ip.parse().unwrap()), "{ip}");
        }
        assert!(!shared("181.45.12.9".parse().unwrap()));
        assert_eq!(UpnpState::Open("181.45.12.9".parse().unwrap()).route(7787).as_deref(), Some("https://181.45.12.9:7787"));
        assert_eq!(UpnpState::Shared("100.72.0.9".parse().unwrap()).route(7787), None);
    }
}

#[cfg(test)]
mod router {
    /// Looks at this network's router without opening anything:
    /// `cargo test -p openide-remote this_networks_router -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "asks the real router on this network"]
    async fn this_networks_router() {
        match super::search_gateway(super::options()).await {
            Ok(gateway) => {
                let external = gateway.get_external_ip().await;
                eprintln!("router {} · external {external:?}", gateway.addr);
                if let Ok(std::net::IpAddr::V4(ip)) = external {
                    eprintln!("shared (CGNAT or second router): {}", super::shared(ip));
                }
            }
            Err(error) => eprintln!("no UPnP router: {error}"),
        }
    }
}
