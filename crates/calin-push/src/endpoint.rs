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
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EndpointError::Credentials);
    }
    if url.fragment().is_some() {
        return Err(EndpointError::Fragment);
    }
    if url.scheme() == "http"
        && policy == EndpointPolicy::AllowLoopbackHttpForDevelopment
        && url.host() == Some(Host::Ipv4(Ipv4Addr::LOCALHOST))
        && url.port().is_some()
    {
        return Ok(url);
    }
    if url.scheme() != "https" {
        return Err(EndpointError::NotHttps);
    }
    // `url` already drops an explicit `:443`, so any port left is another one.
    if url.port().is_some() {
        return Err(EndpointError::Port);
    }
    match url.host() {
        Some(Host::Domain(domain)) if is_public_name(domain) => Ok(url),
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

fn is_public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_multicast()
        || a == 0
        // Shared address space (carrier-grade NAT), RFC 6598.
        || (a == 100 && (64..128).contains(&b))
        // IETF protocol assignments, RFC 6890.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking, RFC 2544.
        || (a == 198 && (18..20).contains(&b))
        // Reserved for future use.
        || a >= 240)
}

fn is_public_v6(address: Ipv6Addr) -> bool {
    if let Some(v4) = address.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let segments = address.segments();
    // NAT64 (RFC 6052) reaches the IPv4 address in its last 32 bits.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., high, low] = segments;
        let embedded = (u32::from(high) << 16) | u32::from(low);
        return is_public_v4(Ipv4Addr::from(embedded));
    }
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || address.is_unique_local()
        || address.is_unicast_link_local()
        // IPv4-compatible (deprecated) and other addresses in ::/96.
        || segments[..6] == [0; 6]
        // Documentation, RFC 3849.
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
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
}
