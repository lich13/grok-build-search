use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use grok_build_search_mcp::{ErrorCode, validate_url_with_resolved_ips};

#[test]
fn rejects_non_http_schemes() {
    let error =
        validate_url_with_resolved_ips("file:///etc/passwd", &[]).expect_err("file URLs must fail");

    assert_eq!(error.code, ErrorCode::InvalidUrl);
}

#[test]
fn rejects_url_userinfo() {
    let addresses = [IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))];
    let error =
        validate_url_with_resolved_ips("https://admin:secret@example.com/private", &addresses)
            .expect_err("userinfo must fail");

    assert_eq!(error.code, ErrorCode::InvalidUrl);
}

#[test]
fn rejects_localhost_and_private_literal_addresses() {
    let cases = [
        "http://localhost/admin",
        "http://api.localhost/admin",
        "http://127.0.0.1/admin",
        "http://169.254.169.254/latest/meta-data",
        "http://10.0.0.1/admin",
        "http://[::1]/admin",
        "http://[fc00::1]/admin",
    ];

    for url in cases {
        let error =
            validate_url_with_resolved_ips(url, &[]).expect_err("local/private URL must fail");
        assert_eq!(
            error.code,
            ErrorCode::PrivateUrl,
            "unexpected code for {url}"
        );
    }
}

#[test]
fn rejects_domain_when_any_resolved_address_is_private() {
    let addresses = [
        IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
    ];

    let error = validate_url_with_resolved_ips("https://example.com/page", &addresses)
        .expect_err("mixed public/private DNS results must fail");

    assert_eq!(error.code, ErrorCode::PrivateUrl);
}

#[test]
fn rejects_documentation_and_multicast_ranges() {
    let cases = [
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)),
        IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().unwrap()),
        IpAddr::V6("ff02::1".parse::<Ipv6Addr>().unwrap()),
    ];

    for address in cases {
        let error = validate_url_with_resolved_ips("https://example.com/page", &[address])
            .expect_err("reserved address must fail");
        assert_eq!(error.code, ErrorCode::PrivateUrl);
    }
}

#[test]
fn accepts_public_https_url_when_all_addresses_are_public() {
    let addresses = [
        IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
        IpAddr::V6("2606:2800:220:1:248:1893:25c8:1946".parse().unwrap()),
    ];

    let parsed = validate_url_with_resolved_ips("https://example.com/path?q=rust", &addresses)
        .expect("public URL should pass");

    assert_eq!(parsed.as_str(), "https://example.com/path?q=rust");
}
