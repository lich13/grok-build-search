use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use tokio::net::lookup_host;
use url::{Host, Url};

use crate::{ErrorCode, ToolError};

pub fn validate_url_with_resolved_ips(
    input: &str,
    resolved_addresses: &[IpAddr],
) -> Result<Url, ToolError> {
    let parsed = validate_url_structure(input)?;
    match parsed.host() {
        Some(Host::Ipv4(address)) => ensure_public_address(IpAddr::V4(address))?,
        Some(Host::Ipv6(address)) => ensure_public_address(IpAddr::V6(address))?,
        Some(Host::Domain(host)) if is_localhost(host) => {
            return Err(private_url_error());
        }
        Some(Host::Domain(_)) => {
            if resolved_addresses.is_empty() {
                return Err(ToolError::new(
                    ErrorCode::InvalidUrl,
                    "URL host did not resolve to an address",
                ));
            }
            for address in resolved_addresses {
                ensure_public_address(*address)?;
            }
        }
        None => unreachable!("validate_url_structure requires a host"),
    }
    Ok(parsed)
}

pub async fn validate_public_url(input: &str) -> Result<Url, ToolError> {
    let parsed = validate_url_structure(input)?;
    match parsed.host() {
        Some(Host::Ipv4(address)) => {
            ensure_public_address(IpAddr::V4(address))?;
            Ok(parsed)
        }
        Some(Host::Ipv6(address)) => {
            ensure_public_address(IpAddr::V6(address))?;
            Ok(parsed)
        }
        Some(Host::Domain(host)) if is_localhost(host) => Err(private_url_error()),
        Some(Host::Domain(host)) => {
            let port = parsed
                .port_or_known_default()
                .ok_or_else(|| ToolError::new(ErrorCode::InvalidUrl, "URL has no usable port"))?;
            let addresses: Vec<IpAddr> = lookup_host((host, port))
                .await
                .map_err(|error| {
                    ToolError::new(
                        ErrorCode::InvalidUrl,
                        format!("could not resolve URL host: {error}"),
                    )
                })?
                .map(|socket| socket.ip())
                .collect();
            validate_url_with_resolved_ips(input, &addresses)
        }
        None => unreachable!("validate_url_structure requires a host"),
    }
}

fn validate_url_structure(input: &str) -> Result<Url, ToolError> {
    let parsed = Url::parse(input)
        .map_err(|error| ToolError::new(ErrorCode::InvalidUrl, format!("invalid URL: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ToolError::new(
            ErrorCode::InvalidUrl,
            "URL scheme must be http or https",
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ToolError::new(
            ErrorCode::InvalidUrl,
            "URL userinfo is not allowed",
        ));
    }
    if parsed.host().is_none() {
        return Err(ToolError::new(
            ErrorCode::InvalidUrl,
            "URL must include a host",
        ));
    }
    Ok(parsed)
}

fn is_localhost(host: &str) -> bool {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    normalized == "localhost" || normalized.ends_with(".localhost")
}

fn ensure_public_address(address: IpAddr) -> Result<(), ToolError> {
    if is_public_address(address) {
        Ok(())
    } else {
        Err(private_url_error())
    }
}

pub(crate) fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

pub(crate) fn is_safe_source_url(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(address)) => is_public_ipv4(address),
        Some(Host::Ipv6(address)) => is_public_ipv6(address),
        Some(Host::Domain(host)) => !is_localhost(host),
        None => false,
    }
}

fn private_url_error() -> ToolError {
    ToolError::new(
        ErrorCode::PrivateUrl,
        "URL host resolves to a local, private, or reserved address",
    )
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _d] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (18..=19).contains(&b))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = address.segments();
    let is_non_public = address.is_unspecified()
        || address.is_loopback()
        || (segments[0] & 0xe000) != 0x2000
        || (segments[0] == 0x2001 && segments[1] == 0x0db8);
    !is_non_public
}
