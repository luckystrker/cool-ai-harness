"""Tests for SSRF protection (Фаза 1.5 §2)."""

from __future__ import annotations

from app.security.ssrf import check_url_safety, is_private_ip, is_safe_url


class TestIsPrivateIp:
    def test_loopback_v4(self) -> None:
        assert is_private_ip("127.0.0.1") is True

    def test_loopback_v6(self) -> None:
        assert is_private_ip("::1") is True

    def test_rfc1918_10(self) -> None:
        assert is_private_ip("10.0.0.1") is True

    def test_rfc1918_172(self) -> None:
        assert is_private_ip("172.16.0.1") is True

    def test_rfc1918_192(self) -> None:
        assert is_private_ip("192.168.1.1") is True

    def test_link_local(self) -> None:
        assert is_private_ip("169.254.1.1") is True

    def test_public_ip(self) -> None:
        assert is_private_ip("8.8.8.8") is False

    def test_invalid_ip(self) -> None:
        assert is_private_ip("not-an-ip") is False


class TestIsSafeUrl:
    def test_rejects_non_http_scheme(self) -> None:
        result = is_safe_url("file:///etc/passwd")
        assert not result.safe
        assert "scheme" in result.reason.lower()

    def test_rejects_ftp_scheme(self) -> None:
        result = is_safe_url("ftp://example.com/file")
        assert not result.safe

    def test_rejects_missing_hostname(self) -> None:
        result = is_safe_url("http:///path")
        assert not result.safe

    def test_allows_https(self) -> None:
        result = is_safe_url("https://example.com/page")
        assert result.safe

    def test_allows_http(self) -> None:
        result = is_safe_url("http://example.com/page")
        assert result.safe

    def test_blocks_private_ip_literal(self) -> None:
        result = is_safe_url("http://127.0.0.1/admin")
        assert not result.safe
        assert result.blocked_ip == "127.0.0.1"

    def test_blocks_10_ip_literal(self) -> None:
        result = is_safe_url("http://10.0.0.1/internal")
        assert not result.safe

    def test_allows_public_ip_literal(self) -> None:
        result = is_safe_url("http://8.8.8.8/dns")
        assert result.safe

    def test_domain_allowlist_allows_exact(self) -> None:
        result = is_safe_url(
            "https://api.github.com/repos",
            allowed_domains=["api.github.com"],
        )
        assert result.safe

    def test_domain_allowlist_allows_subdomain(self) -> None:
        result = is_safe_url(
            "https://sub.example.com/page",
            allowed_domains=["example.com"],
        )
        assert result.safe

    def test_domain_allowlist_rejects_non_listed(self) -> None:
        result = is_safe_url(
            "https://evil.com/page",
            allowed_domains=["example.com"],
        )
        assert not result.safe
        assert "not in allowlist" in result.reason.lower()

    def test_domain_allowlist_case_insensitive(self) -> None:
        result = is_safe_url(
            "https://API.GitHub.com/repos",
            allowed_domains=["api.github.com"],
        )
        assert result.safe

    def test_block_private_ips_false_allows_localhost(self) -> None:
        result = is_safe_url("http://127.0.0.1/", block_private_ips=False)
        assert result.safe

    def test_check_url_safety_alias(self) -> None:
        """check_url_safety should behave identically to is_safe_url."""
        r1 = check_url_safety("https://example.com/")
        r2 = is_safe_url("https://example.com/")
        assert r1.safe == r2.safe
