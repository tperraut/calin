//! The address a request is attributed to (spec section 3).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// The client address of a request: the peer address, or the last
/// `X-Forwarded-For` entry when the peer is loopback, because then the peer
/// is the reverse proxy and that entry is what the proxy itself appended.
///
/// `forwarded_for` is every `X-Forwarded-For` header value of the request, in
/// order. Earlier entries are whatever the client claimed and are never
/// used. A last entry that is not an address leaves the peer address: all
/// such requests then share the loopback bucket, the limiting side.
///
/// A header on a request whose peer is not loopback is ignored entirely.
pub fn client_address<'a>(
    peer: IpAddr,
    forwarded_for: impl IntoIterator<Item = &'a str>,
) -> IpAddr {
    // A dual-stack listener reports IPv4 peers as ::ffff:a.b.c.d.
    let peer = peer.to_canonical();
    if !peer.is_loopback() {
        return peer;
    }
    forwarded_for
        .into_iter()
        .last()
        .and_then(|value| value.rsplit(',').next())
        .and_then(parse_entry)
        .unwrap_or(peer)
}

/// What a client address is counted under: the address itself for IPv4,
/// the /64 for IPv6. A /64 is the smallest block one customer usually
/// controls, so counting single IPv6 addresses would limit nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AddressKey {
    V4(Ipv4Addr),
    V6Prefix(u64),
}

impl AddressKey {
    pub(crate) fn of(address: IpAddr) -> Self {
        match address.to_canonical() {
            IpAddr::V4(v4) => Self::V4(v4),
            // The upper 64 bits: the network prefix.
            IpAddr::V6(v6) => Self::V6Prefix((u128::from(v6) >> 64) as u64),
        }
    }
}

/// An address as proxies write it: bare, `[v6]`, or with a port.
fn parse_entry(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim_matches([' ', '\t']);
    if let Ok(address) = entry.parse::<IpAddr>() {
        return Some(address.to_canonical());
    }
    if let Ok(socket) = entry.parse::<SocketAddr>() {
        return Some(socket.ip().to_canonical());
    }
    let bracketed = entry.strip_prefix('[')?.strip_suffix(']')?;
    bracketed.parse::<IpAddr>().ok().map(|a| a.to_canonical())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn client_address_table() {
        let cases: &[(&str, &[&str], &str)] = &[
            // A peer that is not loopback is the client, whatever it claims.
            ("203.0.113.9", &[], "203.0.113.9"),
            ("203.0.113.9", &["198.51.100.1"], "203.0.113.9"),
            ("203.0.113.9", &["127.0.0.1"], "203.0.113.9"),
            ("10.0.0.5", &["198.51.100.1"], "10.0.0.5"),
            ("2001:db8::1", &["198.51.100.1"], "2001:db8::1"),
            // Loopback without a header: a local process.
            ("127.0.0.1", &[], "127.0.0.1"),
            ("::1", &[], "::1"),
            // Loopback with a header: the last entry, which the proxy wrote.
            ("127.0.0.1", &["198.51.100.1"], "198.51.100.1"),
            ("::1", &["198.51.100.1"], "198.51.100.1"),
            ("127.0.0.1", &["10.9.9.9, 198.51.100.1"], "198.51.100.1"),
            ("127.0.0.1", &["10.9.9.9,198.51.100.1"], "198.51.100.1"),
            (
                "127.0.0.1",
                &["1.1.1.1, 2.2.2.2", "3.3.3.3, 198.51.100.1"],
                "198.51.100.1",
            ),
            (
                "127.0.0.1",
                &["198.51.100.1", "203.0.113.77"],
                "203.0.113.77",
            ),
            ("127.0.0.1", &[" 198.51.100.1\t"], "198.51.100.1"),
            ("127.0.0.1", &["2001:db8::7"], "2001:db8::7"),
            ("127.0.0.1", &["[2001:db8::7]"], "2001:db8::7"),
            ("127.0.0.1", &["[2001:db8::7]:4711"], "2001:db8::7"),
            ("127.0.0.1", &["198.51.100.1:4711"], "198.51.100.1"),
            ("127.0.0.1", &["::ffff:198.51.100.1"], "198.51.100.1"),
            ("127.8.8.8", &["198.51.100.1"], "198.51.100.1"),
            // An IPv4 loopback peer seen through a dual-stack socket.
            ("::ffff:127.0.0.1", &["198.51.100.1"], "198.51.100.1"),
            ("::ffff:203.0.113.9", &["198.51.100.1"], "203.0.113.9"),
            // A spoofed first entry never wins over the proxy's own.
            ("127.0.0.1", &["127.0.0.1, 198.51.100.1"], "198.51.100.1"),
            // A last entry that is not an address: fall back to the peer.
            ("127.0.0.1", &["198.51.100.1, unknown"], "127.0.0.1"),
            ("127.0.0.1", &["198.51.100.1,"], "127.0.0.1"),
            ("127.0.0.1", &[""], "127.0.0.1"),
            ("127.0.0.1", &["198.51.100.1", ""], "127.0.0.1"),
            ("127.0.0.1", &["_hidden"], "127.0.0.1"),
            ("127.0.0.1", &["198.51.100.1 198.51.100.2"], "127.0.0.1"),
            ("127.0.0.1", &["[198.51.100.1"], "127.0.0.1"),
            ("::1", &["999.1.1.1"], "::1"),
        ];
        for (peer, headers, expected) in cases {
            assert_eq!(
                client_address(ip(peer), headers.iter().copied()),
                ip(expected),
                "peer {peer}, headers {headers:?}"
            );
        }
    }

    #[test]
    fn address_keys_table() {
        let same = [
            ("198.51.100.1", "198.51.100.1"),
            ("198.51.100.1", "::ffff:198.51.100.1"),
            ("2001:db8:1:2::1", "2001:db8:1:2:ffff:ffff:ffff:ffff"),
            ("::1", "::2"),
        ];
        for (a, b) in same {
            assert_eq!(AddressKey::of(ip(a)), AddressKey::of(ip(b)), "{a} {b}");
        }
        let different = [
            ("198.51.100.1", "198.51.100.2"),
            ("2001:db8:1:2::1", "2001:db8:1:3::1"),
            ("127.0.0.1", "::1"),
            ("0.0.0.1", "::1"),
        ];
        for (a, b) in different {
            assert_ne!(AddressKey::of(ip(a)), AddressKey::of(ip(b)), "{a} {b}");
        }
    }
}
