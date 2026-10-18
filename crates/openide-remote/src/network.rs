//! Where the PC can be reached from a phone.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket};

/// The address the PC's default route leaves from. A UDP socket that
/// "connects" only picks a route; nothing is sent.
fn route_source(bind: &str, target: &str) -> Option<IpAddr> {
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(target).ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

/// The PC's address on its local network.
pub fn lan_ipv4() -> Option<Ipv4Addr> {
    match route_source("0.0.0.0:0", "1.1.1.1:9")? {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// A global IPv6 address, which a phone outside the house reaches without
/// any port being opened on IPv4 (only the router's IPv6 firewall stands).
pub fn global_ipv6() -> Option<Ipv6Addr> {
    match route_source("[::]:0", "[2606:4700:4700::1111]:9")? {
        IpAddr::V6(ip) if ip.segments()[0] & 0xe000 == 0x2000 => Some(ip),
        _ => None,
    }
}

/// `https://` addresses for the pairing code, the likeliest first: the
/// local network, then IPv6, then `public` (a name or address the person
/// gave, `host` or `host:port`, for a port opened on their router).
pub fn routes(port: u16, public: Option<&str>) -> Vec<String> {
    let mut routes = Vec::new();
    if let Some(ip) = lan_ipv4() {
        routes.push(format!("https://{ip}:{port}"));
    }
    if let Some(ip) = global_ipv6() {
        routes.push(format!("https://[{ip}]:{port}"));
    }
    if let Some(public) = public.map(str::trim).filter(|public| !public.is_empty()) {
        let public = public.trim_start_matches("https://").trim_end_matches('/');
        let has_port = public.rsplit_once(':').is_some_and(|(host, port)| {
            !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) && (!host.contains(':') || host.ends_with(']'))
        });
        routes.push(if has_port { format!("https://{public}") } else { format!("https://{public}:{port}") });
    }
    routes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_public_address_gets_the_port_unless_it_has_one() {
        let last = |public: &str| routes(7787, Some(public)).pop().unwrap();
        assert_eq!(last("casa.example.com"), "https://casa.example.com:7787");
        assert_eq!(last("https://casa.example.com:9000/"), "https://casa.example.com:9000");
        assert_eq!(last("[2800::1]:9000"), "https://[2800::1]:9000");
    }
}
