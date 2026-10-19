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
/// The stable one of the network the PC goes out by: the one it goes out
/// with is a privacy address, replaced every day.
pub fn global_ipv6() -> Option<Ipv6Addr> {
    let outgoing = match route_source("[::]:0", "[2606:4700:4700::1111]:9")? {
        IpAddr::V6(ip) if global(ip) => ip,
        _ => return None,
    };
    let stable = std::fs::read_to_string("/proc/net/if_inet6")
        .ok()
        .and_then(|table| stable_ipv6(&table, outgoing));
    Some(stable.unwrap_or(outgoing))
}

fn global(ip: Ipv6Addr) -> bool {
    ip.segments()[0] & 0xe000 == 0x2000
}

/// From Linux's address table (address, index, prefix, scope and flags in
/// hex per line), a global address of `outgoing`'s /64 that is neither a
/// temporary one nor deprecated.
fn stable_ipv6(table: &str, outgoing: Ipv6Addr) -> Option<Ipv6Addr> {
    const TEMPORARY: u32 = 0x01;
    const DEPRECATED: u32 = 0x20;
    const TENTATIVE: u32 = 0x40;
    table.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let hex = fields.first().filter(|hex| hex.len() == 32)?;
        let ip = Ipv6Addr::from(u128::from_str_radix(hex, 16).ok()?);
        let flags = u32::from_str_radix(fields.get(4)?, 16).ok()?;
        let same_network = ip.segments()[..4] == outgoing.segments()[..4];
        (global(ip) && same_network && flags & (TEMPORARY | DEPRECATED | TENTATIVE) == 0).then_some(ip)
    })
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
    fn the_stable_ipv6_address_is_chosen_over_the_daily_privacy_one() {
        // As the developer's laptop listed them on 2026-10-18.
        let table = "28039810b5de1210144f78d1a1da6b23 02 40 00 00   wlp3s0\n\
                     fdd648e3eb510010e4e6e6d9773ca514 02 40 00 01   wlp3s0\n\
                     28039810b5de12108374950883edf20c 02 40 00 01   wlp3s0\n\
                     fdd648e3eb510010fa1896f202be8de6 02 40 00 80   wlp3s0\n";
        let outgoing: Ipv6Addr = "2803:9810:b5de:1210:8374:9508:83ed:f20c".parse().unwrap();
        assert_eq!(stable_ipv6(table, outgoing), Some("2803:9810:b5de:1210:144f:78d1:a1da:6b23".parse().unwrap()));
        let elsewhere: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert_eq!(stable_ipv6(table, elsewhere), None);
    }

    #[test]
    fn a_public_address_gets_the_port_unless_it_has_one() {
        let last = |public: &str| routes(7787, Some(public)).pop().unwrap();
        assert_eq!(last("casa.example.com"), "https://casa.example.com:7787");
        assert_eq!(last("https://casa.example.com:9000/"), "https://casa.example.com:9000");
        assert_eq!(last("[2800::1]:9000"), "https://[2800::1]:9000");
    }
}
