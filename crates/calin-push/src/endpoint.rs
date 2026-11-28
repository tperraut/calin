//! Which push endpoints the server will call.
//!
//! An endpoint is a URL a signed-in user hands the server, which the server
//! then requests: without these rules it would be a way to make the server
//! reach its own network. Push services are public HTTPS hosts on port 443,
//! so anything else is refused before a request is made, and a name that
//! resolves only to non-public addresses is refused when connecting.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// The longest endpoint accepted. Real ones are a few hundred characters.
pub const MAX_ENDPOINT_LEN: usize = 2048;

/// Whether plain `http` to the loopback address is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EndpointPolicy {
    /// Only public `https` endpoints. The only setting for a real server.
    #[default]
    PublicHttpsOnly,
    /// Also `http://127.0.0.1:<port>`, for tests and development with a fake
    /// push service.
    AllowLoopbackHttpForDevelopment,
}

/// Why an endpoint was refused. Safe to show to the user who sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    /// Longer than [`MAX_ENDPOINT_LEN`].
    #[error("the endpoint is longer than {MAX_ENDPOINT_LEN} characters")]
    TooLong,
    /// Not a URL.
    #[error("the endpoint is not a URL")]
    NotUrl,
    /// Not `https`.
    #[error("the endpoint must be an https URL")]
    NotHttps,
    /// Has a user name or password.
    #[error("the endpoint must not have credentials")]
    Credentials,
    /// Names a port other than 443.
    #[error("the endpoint must use the default port")]
    Port,
    /// Has a fragment, which would never be sent.
    #[error("the endpoint must not have a fragment")]
    Fragment,
    /// An IP address, `localhost`, a single label, or a private suffix.
    #[error("the endpoint's host must be a public domain name")]
    Host,
}

/// Parses and checks an endpoint. Returns the URL the server will request.
pub fn validate_endpoint(endpoint: &str, policy: EndpointPolicy) -> Result<Url, EndpointError> {
    if endpoint.len() > MAX_ENDPOINT_LEN {
        return Err(EndpointError::TooLong);
    }
    let url = Url::parse(endpoint).map_err(|_| EndpointError::NotUrl)?;
    check_no_extras(&url)?;
    if !is_development_loopback(&url, policy) {
        check_public_https(&url)?;
    }
    Ok(url)
}

/// Credentials and a fragment are refused whatever the policy.
fn check_no_extras(url: &Url) -> Result<(), EndpointError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EndpointError::Credentials);
    }
    if url.fragment().is_some() {
        return Err(EndpointError::Fragment);
    }
    Ok(())
}

/// `http://127.0.0.1:<port>`, under the development policy only.
fn is_development_loopback(url: &Url, policy: EndpointPolicy) -> bool {
    url.scheme() == "http"
        && policy == EndpointPolicy::AllowLoopbackHttpForDevelopment
        && url.host() == Some(Host::Ipv4(Ipv4Addr::LOCALHOST))
        && url.port().is_some()
}

fn check_public_https(url: &Url) -> Result<(), EndpointError> {
    if url.scheme() != "https" {
        return Err(EndpointError::NotHttps);
    }
    // `url` already drops an explicit `:443`, so any port left is another one.
    if url.port().is_some() {
        return Err(EndpointError::Port);
    }
    match url.host() {
        Some(Host::Domain(domain)) if is_public_name(domain) => Ok(()),
        _ => Err(EndpointError::Host),
    }
}

/// A name that can only be public: at least two labels, and not one of the
/// names reserved for a machine or its private network.
fn is_public_name(domain: &str) -> bool {
    // A trailing dot is the same name, fully qualified.
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    let private_suffix = [".localhost", ".local", ".internal", ".home.arpa", ".lan"]
        .iter()
        .any(|suffix| domain.ends_with(suffix));
    !domain.is_empty()
        && domain.contains('.')
        && domain != "localhost"
        && !private_suffix
        && !domain.starts_with('.')
        && !domain.contains("..")
}

/// Whether an address is reachable on the public internet, as far as the
/// reserved ranges tell. Used on every address a host name resolves to.
pub(crate) fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// A block of addresses: its first address and its prefix length, as in
/// `10.0.0.0/8`.
type Block<A> = (A, u32);

/// The IPv4 blocks that are not on the public internet.
const NON_PUBLIC_V4: [Block<Ipv4Addr>; 14] = [
    // "This network", 0.0.0.0 included.
    (Ipv4Addr::new(0, 0, 0, 0), 8),
    // Private, RFC 1918.
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    // Shared address space (carrier-grade NAT), RFC 6598.
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    // Loopback.
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    // Link local.
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    // Private, RFC 1918.
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    // IETF protocol assignments, RFC 6890.
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    // Documentation, RFC 5737.
    (Ipv4Addr::new(192, 0, 2, 0), 24),
    // Private, RFC 1918.
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    // Benchmarking, RFC 2544.
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    // Documentation, RFC 5737.
    (Ipv4Addr::new(198, 51, 100, 0), 24),
    // Documentation, RFC 5737.
    (Ipv4Addr::new(203, 0, 113, 0), 24),
    // Multicast.
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    // Reserved for future use, and broadcast.
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];

/// The IPv6 blocks that are not on the public internet.
const NON_PUBLIC_V6: [Block<Ipv6Addr>; 5] = [
    // Unspecified, loopback, and IPv4-compatible (deprecated).
    (Ipv6Addr::UNSPECIFIED, 96),
    // Documentation, RFC 3849.
    (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32),
    // Unique local.
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
    // Link local.
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
    // Multicast.
    (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8),
];

/// The IPv6 blocks whose last 32 bits are the IPv4 address actually
/// reached: IPv4-mapped, and NAT64 (RFC 6052).
const EMBEDS_V4: [Block<Ipv6Addr>; 2] = [
    (Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96),
    (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96),
];

fn is_public_v4(address: Ipv4Addr) -> bool {
    let bits = u128::from(u32::from(address));
    !NON_PUBLIC_V4
        .iter()
        .any(|&(first, prefix)| same_prefix(bits, u32::from(first).into(), 32 - prefix))
}

fn is_public_v6(address: Ipv6Addr) -> bool {
    let in_any = |blocks: &[Block<Ipv6Addr>]| {
        blocks
            .iter()
            .any(|&(first, prefix)| same_prefix(address.into(), first.into(), 128 - prefix))
    };
    if in_any(&EMBEDS_V4) {
        let [.., a, b, c, d] = address.octets();
        return is_public_v4(Ipv4Addr::new(a, b, c, d));
    }
    !in_any(&NON_PUBLIC_V6)
}

/// Whether `a` and `b` are equal once their last `host_bits` bits are
/// dropped.
fn same_prefix(a: u128, b: u128, host_bits: u32) -> bool {
    a.checked_shr(host_bits) == b.checked_shr(host_bits)
}

/// The origin a VAPID token is addressed to (RFC 8292 section 2): scheme,
/// host and any non-default port.
pub(crate) fn audience(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// The host alone: the only part of an endpoint that may be logged or
/// listed. The rest of the URL is a capability to send to the device.
pub(crate) fn service(url: &Url) -> String {
    url.host_str().unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use EndpointError as E;
    use EndpointPolicy::{AllowLoopbackHttpForDevelopment as Dev, PublicHttpsOnly as Strict};

    #[test]
    fn endpoint_table() {
        let long = format!("https://push.example.net/{}", "a".repeat(MAX_ENDPOINT_LEN));
        type Case<'a> = (&'a str, Result<(), E>, Result<(), E>);
        let cases: &[Case] = &[
            // (endpoint, strict, development)
            (
                "https://fcm.googleapis.com/fcm/send/abc:def",
                Ok(()),
                Ok(()),
            ),
            (
                "https://updates.push.services.mozilla.com/wpush/v2/gAAA",
                Ok(()),
                Ok(()),
            ),
            ("https://web.push.apple.com/QGuA?x=1", Ok(()), Ok(())),
            ("https://push.example.net:443/p", Ok(()), Ok(())),
            ("https://PUSH.Example.NET./p", Ok(()), Ok(())),
            (
                "http://push.example.net/p",
                Err(E::NotHttps),
                Err(E::NotHttps),
            ),
            (
                "ftp://push.example.net/p",
                Err(E::NotHttps),
                Err(E::NotHttps),
            ),
            (
                "https://user:pw@push.example.net/p",
                Err(E::Credentials),
                Err(E::Credentials),
            ),
            (
                "https://user@push.example.net/p",
                Err(E::Credentials),
                Err(E::Credentials),
            ),
            (
                "https://push.example.net:8443/p",
                Err(E::Port),
                Err(E::Port),
            ),
            ("https://push.example.net:80/p", Err(E::Port), Err(E::Port)),
            (
                "https://push.example.net/p#frag",
                Err(E::Fragment),
                Err(E::Fragment),
            ),
            ("https://127.0.0.1/p", Err(E::Host), Err(E::Host)),
            ("https://10.0.0.8/p", Err(E::Host), Err(E::Host)),
            ("https://8.8.8.8/p", Err(E::Host), Err(E::Host)),
            ("https://0x7f.1/p", Err(E::Host), Err(E::Host)),
            ("https://2130706433/p", Err(E::Host), Err(E::Host)),
            ("https://[::1]/p", Err(E::Host), Err(E::Host)),
            ("https://[2606:4700::1]/p", Err(E::Host), Err(E::Host)),
            ("https://localhost/p", Err(E::Host), Err(E::Host)),
            ("https://localhost./p", Err(E::Host), Err(E::Host)),
            ("https://push.localhost/p", Err(E::Host), Err(E::Host)),
            ("https://printer.local/p", Err(E::Host), Err(E::Host)),
            (
                "https://metadata.google.internal/p",
                Err(E::Host),
                Err(E::Host),
            ),
            ("https://router.home.arpa/p", Err(E::Host), Err(E::Host)),
            ("https://intranet/p", Err(E::Host), Err(E::Host)),
            ("not a url", Err(E::NotUrl), Err(E::NotUrl)),
            ("", Err(E::NotUrl), Err(E::NotUrl)),
            ("/relative/path", Err(E::NotUrl), Err(E::NotUrl)),
            (&long, Err(E::TooLong), Err(E::TooLong)),
            // The development switch opens exactly http://127.0.0.1:<port>.
            ("http://127.0.0.1:8080/push/1", Err(E::NotHttps), Ok(())),
            (
                "http://127.0.0.1/push/1",
                Err(E::NotHttps),
                Err(E::NotHttps),
            ),
            (
                "http://localhost:8080/push/1",
                Err(E::NotHttps),
                Err(E::NotHttps),
            ),
            (
                "http://[::1]:8080/push/1",
                Err(E::NotHttps),
                Err(E::NotHttps),
            ),
            (
                "http://10.0.0.1:8080/push/1",
                Err(E::NotHttps),
                Err(E::NotHttps),
            ),
            (
                "http://u:p@127.0.0.1:8080/p",
                Err(E::Credentials),
                Err(E::Credentials),
            ),
            ("https://127.0.0.1:8080/push/1", Err(E::Port), Err(E::Port)),
        ];
        for (endpoint, strict, dev) in cases {
            assert_eq!(
                validate_endpoint(endpoint, Strict).map(|_| ()),
                *strict,
                "strict {endpoint}"
            );
            assert_eq!(
                validate_endpoint(endpoint, Dev).map(|_| ()),
                *dev,
                "dev {endpoint}"
            );
        }
        let exactly = format!(
            "https://push.example.net/{}",
            "a".repeat(MAX_ENDPOINT_LEN - "https://push.example.net/".len())
        );
        assert!(validate_endpoint(&exactly, Strict).is_ok());
    }

    #[test]
    fn audience_is_the_origin_and_service_the_host() {
        let url = validate_endpoint("https://fcm.googleapis.com/fcm/send/x?y=1", Strict).unwrap();
        assert_eq!(audience(&url), "https://fcm.googleapis.com");
        assert_eq!(service(&url), "fcm.googleapis.com");
        let url = validate_endpoint("http://127.0.0.1:4100/p", Dev).unwrap();
        assert_eq!(audience(&url), "http://127.0.0.1:4100");
    }

    #[test]
    fn resolved_addresses_must_be_public() {
        for public in [
            "8.8.8.8",
            "142.250.74.10",
            "2606:4700:4700::1111",
            "2a00:1450:4007:80e::200a",
            "64:ff9b::808:808",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public_address(public.parse().unwrap()), "{public}");
        }
        for private in [
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::127.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_address(private.parse().unwrap()), "{private}");
        }
    }

    #[test]
    fn every_refused_range_ends_where_it_should() {
        // (address, public): the first and last address of each refused
        // range, and the neighbours just outside it.
        let cases = [
            ("0.255.255.255", false),
            ("1.0.0.0", true),
            ("9.255.255.255", true),
            ("10.0.0.0", false),
            ("10.255.255.255", false),
            ("11.0.0.0", true),
            ("100.63.255.255", true),
            ("100.64.0.0", false),
            ("100.127.255.255", false),
            ("100.128.0.0", true),
            ("126.255.255.255", true),
            ("127.0.0.0", false),
            ("127.255.255.255", false),
            ("128.0.0.0", true),
            ("169.253.255.255", true),
            ("169.254.0.0", false),
            ("169.254.255.255", false),
            ("169.255.0.0", true),
            ("172.15.255.255", true),
            ("172.16.0.0", false),
            ("172.31.255.255", false),
            ("172.32.0.0", true),
            ("191.255.255.255", true),
            ("192.0.0.0", false),
            ("192.0.0.255", false),
            ("192.0.1.0", true),
            ("192.0.1.255", true),
            ("192.0.2.0", false),
            ("192.0.2.255", false),
            ("192.0.3.0", true),
            ("192.167.255.255", true),
            ("192.168.0.0", false),
            ("192.168.255.255", false),
            ("192.169.0.0", true),
            ("198.17.255.255", true),
            ("198.18.0.0", false),
            ("198.19.255.255", false),
            ("198.20.0.0", true),
            ("198.51.99.255", true),
            ("198.51.100.0", false),
            ("198.51.100.255", false),
            ("198.51.101.0", true),
            ("203.0.112.255", true),
            ("203.0.113.0", false),
            ("203.0.113.255", false),
            ("203.0.114.0", true),
            ("223.255.255.255", true),
            ("224.0.0.0", false),
            ("239.255.255.255", false),
            ("240.0.0.0", false),
            ("255.255.255.254", false),
            ("255.255.255.255", false),
            // IPv6.
            ("::", false),
            ("::1", false),
            ("::ffff:ffff", false),
            ("::1:0:0", true),
            ("::ffff:8.8.8.8", true),
            ("::ffff:127.0.0.1", false),
            ("::ffff:192.168.0.1", false),
            ("::ffff:100.64.0.1", false),
            ("64:ff9b::808:808", true),
            ("64:ff9b::7f00:1", false),
            ("64:ff9b::c0a8:1", false),
            ("64:ff9b::1:0:0", true),
            ("64:ff9b:1::", true),
            ("2001:db7:ffff:ffff:ffff:ffff:ffff:ffff", true),
            ("2001:db8::", false),
            ("2001:db8:ffff:ffff:ffff:ffff:ffff:ffff", false),
            ("2001:db9::", true),
            ("fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
            ("fc00::", false),
            ("fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
            ("fe00::", true),
            ("fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
            ("fe80::", false),
            ("febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
            ("fec0::", true),
            ("feff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
            ("ff00::", false),
            ("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
        ];
        for (address, public) in cases {
            assert_eq!(
                is_public_address(address.parse().unwrap()),
                public,
                "{address}"
            );
        }
    }
}
