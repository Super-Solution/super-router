//! What the host knows about a customer that can show two accounts are one person: a normalised e-mail, a network
//! prefix, a device id. The host passes them in; they are stored only as keyed hashes.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// One reported fact about a customer, such as `("email", "a@b.c")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub kind: String,
    pub value: String,
}

impl Signal {
    pub fn new(kind: &str, value: &str) -> Self {
        Self { kind: kind.to_owned(), value: value.to_owned() }
    }

    /// An e-mail address in the form [`normalize_email`] gives.
    pub fn email(address: &str) -> Option<Self> {
        normalize_email(address).map(|v| Self::new("email", &v))
    }

    /// A network address reduced to its [`network_prefix`].
    pub fn network(address: &str) -> Option<Self> {
        network_prefix(address).map(|v| Self::new("network", &v))
    }
}

impl Signal {
    /// Shared by many honest people (a network prefix, a campus, a family): enough to notice a crowd of invitees,
    /// never enough to call an invitee the inviter's other account.
    pub fn is_weak(&self) -> bool {
        self.kind == "network"
    }
}

/// The address with the tricks that make one mailbox look like many removed: lower case, no `+tag`, and for Gmail
/// no dots and one domain. `None` when it has no `@`.
pub fn normalize_email(address: &str) -> Option<String> {
    let lower = address.trim().to_lowercase();
    let (local, domain) = lower.rsplit_once('@')?;
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    let local = local.split('+').next().unwrap_or(local);
    let (local, domain) = if domain == "gmail.com" || domain == "googlemail.com" {
        (local.replace('.', ""), "gmail.com")
    } else {
        (local.to_owned(), domain)
    };
    if local.is_empty() {
        return None;
    }
    Some(format!("{local}@{domain}"))
}

/// The /24 of an IPv4 address or the /64 of an IPv6 one, so a person on a changing address still matches
/// themselves. `None` for text that is not an address.
pub fn network_prefix(address: &str) -> Option<String> {
    let a = address.trim();
    if let Ok(v4) = a.parse::<std::net::Ipv4Addr>() {
        let o = v4.octets();
        return Some(format!("{}.{}.{}.0/24", o[0], o[1], o[2]));
    }
    if let Ok(v6) = a.parse::<std::net::Ipv6Addr>() {
        let s = v6.segments();
        return Some(format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3]));
    }
    None
}

/// The stored form of a signal: HMAC-SHA256 of `kind:value` under the host's secret.
pub(crate) fn digest(key: &[u8], s: &Signal) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(b"signal:");
    mac.update(s.kind.as_bytes());
    mac.update(b":");
    mac.update(s.value.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_tricks_collapse_to_one_address() {
        assert_eq!(normalize_email(" Zou.Foo+promo@Gmail.com ").as_deref(), Some("zoufoo@gmail.com"));
        assert_eq!(normalize_email("zoufoo@googlemail.com").as_deref(), Some("zoufoo@gmail.com"));
        assert_eq!(normalize_email("A.B+x@example.org").as_deref(), Some("a.b@example.org"));
        assert_eq!(normalize_email("nope"), None);
        assert_eq!(normalize_email("+x@example.org"), None);
    }

    #[test]
    fn networks_keep_a_prefix_not_an_address() {
        assert_eq!(network_prefix("203.0.113.77").as_deref(), Some("203.0.113.0/24"));
        assert_eq!(network_prefix("2001:db8:1:2:3:4:5:6").as_deref(), Some("2001:db8:1:2::/64"));
        assert_eq!(network_prefix("not an ip"), None);
    }
}
