//! Serial codes and invite codes: generation, normalisation and keyed hashing. Pure: randomness is passed in.
//!
//! A serial code is 11 random symbols of Crockford's base32 (55 bits) and one check symbol, shown as
//! `XXXX-XXXX-XXXX`. Typing is forgiving (case, spaces, hyphens, `O`/`0`, `I`/`L`/`1`) and the check symbol
//! rejects a mistyped code before it is looked up, so a typo is not counted as a guess. Codes are stored only as
//! an HMAC under a host secret: a copy of the table does not give out the codes, and 55 bits cannot be searched
//! offline without that secret.

use hmac::{Hmac, Mac};
use sha2::Sha256;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Random symbols in a serial code.
pub const SERIAL_BODY: usize = 11;
/// All symbols in a serial code, including the check symbol.
pub const SERIAL_LEN: usize = SERIAL_BODY + 1;
/// Symbols in an invite code.
pub const INVITE_LEN: usize = 8;
/// Random bytes [`generate_serial`] needs.
pub const SERIAL_BYTES: usize = 7;
/// Random bytes [`generate_invite`] needs.
pub const INVITE_BYTES: usize = 5;

fn symbol(v: u8) -> char {
    ALPHABET[usize::from(v & 31)] as char
}

fn value(c: char) -> Option<u8> {
    let c = match c.to_ascii_uppercase() {
        'O' => '0',
        'I' | 'L' => '1',
        other => other,
    };
    ALPHABET.iter().position(|a| *a as char == c).map(|p| p as u8)
}

/// Odd weights, so any single changed symbol changes the sum modulo 32.
fn check_symbol(body: &[u8]) -> u8 {
    let mut sum: u32 = 0;
    for (i, v) in body.iter().enumerate() {
        sum += (2 * i as u32 + 1) * u32::from(*v);
    }
    (sum % 32) as u8
}

/// Takes `n` 5-bit values from `bytes`, most significant bit first.
fn five_bit(bytes: &[u8], n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut acc: u64 = 0;
    let mut bits = 0;
    for b in bytes {
        acc = (acc << 8) | u64::from(*b);
        bits += 8;
        while bits >= 5 && out.len() < n {
            bits -= 5;
            out.push(((acc >> bits) & 31) as u8);
        }
    }
    out
}

/// A new serial code in its canonical form (12 symbols, no hyphens) from at least [`SERIAL_BYTES`] random bytes.
pub fn generate_serial(random: &[u8]) -> String {
    assert!(random.len() >= SERIAL_BYTES, "serial codes need {SERIAL_BYTES} random bytes");
    let body = five_bit(random, SERIAL_BODY);
    let check = check_symbol(&body);
    body.iter().copied().chain(std::iter::once(check)).map(symbol).collect()
}

/// A new invite code (8 symbols, 40 bits) from at least [`INVITE_BYTES`] random bytes.
pub fn generate_invite(random: &[u8]) -> String {
    assert!(random.len() >= INVITE_BYTES, "invite codes need {INVITE_BYTES} random bytes");
    five_bit(random, INVITE_LEN).into_iter().map(symbol).collect()
}

/// `XXXX-XXXX-XXXX` for a canonical serial code.
pub fn display_serial(canonical: &str) -> String {
    canonical.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join("-")
}

fn strip(input: &str) -> String {
    input.chars().filter(|c| !c.is_whitespace() && *c != '-' && *c != '_').collect()
}

/// The canonical serial code a person typed, or `None` when it is not one: wrong length, a symbol outside the
/// alphabet, or a check symbol that does not match (a typo).
pub fn normalize_serial(input: &str) -> Option<String> {
    let s = strip(input);
    if s.chars().count() != SERIAL_LEN {
        return None;
    }
    let values: Vec<u8> = s.chars().map(value).collect::<Option<_>>()?;
    let (body, check) = values.split_at(SERIAL_BODY);
    if check_symbol(body) != check[0] {
        return None;
    }
    Some(values.into_iter().map(symbol).collect())
}

/// The canonical invite code a person typed, or `None`.
pub fn normalize_invite(input: &str) -> Option<String> {
    let s = strip(input);
    if s.chars().count() != INVITE_LEN {
        return None;
    }
    let values: Vec<u8> = s.chars().map(value).collect::<Option<_>>()?;
    Some(values.into_iter().map(symbol).collect())
}

/// A custom (chosen) code such as a campaign word: letters and digits, 4 to 32, upper-cased; spaces and hyphens are
/// ignored so `spring-2027` and `SPRING 2027` are one code. `None` for anything else.
pub fn normalize_custom(input: &str) -> Option<String> {
    let s = strip(input).to_ascii_uppercase();
    let len = s.chars().count();
    if !(4..=32).contains(&len) || !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(s)
}

/// The two canonical forms a typed string may mean, serial first.
pub fn candidates(input: &str) -> (Option<String>, Option<String>) {
    (normalize_serial(input), normalize_custom(input))
}

/// HMAC-SHA256 of `kind:canonical` under `key`. `kind` keeps the two code families from colliding.
pub fn hash(key: &[u8], kind: &str, canonical: &str) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(kind.as_bytes());
    mac.update(b":");
    mac.update(canonical.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// What an administrator may see of a stored code: its last four symbols.
pub fn hint(canonical: &str) -> String {
    let n = canonical.chars().count();
    canonical.chars().skip(n.saturating_sub(4)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_serial_normalises_to_itself_and_survives_typing_noise() {
        let code = generate_serial(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde]);
        assert_eq!(code.len(), SERIAL_LEN);
        assert_eq!(normalize_serial(&code).as_deref(), Some(code.as_str()));
        let shown = display_serial(&code);
        assert_eq!(shown.len(), 14);
        assert_eq!(normalize_serial(&shown.to_lowercase()).as_deref(), Some(code.as_str()));
        assert_eq!(normalize_serial(&format!("  {shown}  ")).as_deref(), Some(code.as_str()));
    }

    #[test]
    fn a_single_wrong_symbol_is_always_caught_by_the_check_symbol() {
        let code = generate_serial(&[7, 200, 31, 99, 4, 250, 1]);
        for i in 0..SERIAL_LEN {
            for a in ALPHABET {
                let mut chars: Vec<char> = code.chars().collect();
                if chars[i] as u8 == *a {
                    continue;
                }
                chars[i] = *a as char;
                let typo: String = chars.into_iter().collect();
                assert_eq!(normalize_serial(&typo), None, "{typo} at {i}");
            }
        }
    }

    #[test]
    fn lookalike_letters_are_forgiven_and_wrong_lengths_are_not() {
        // 'O' reads as '0', 'I' and 'L' as '1'.
        assert_eq!(value('o'), Some(0));
        assert_eq!(value('i'), Some(1));
        assert_eq!(value('l'), Some(1));
        assert_eq!(value('U'), None);
        assert_eq!(normalize_serial("ABC"), None);
        assert_eq!(normalize_serial(""), None);
        assert_eq!(normalize_invite("ABCD-EFGH").as_deref(), Some("ABCDEFGH"));
        assert_eq!(normalize_invite("ABCDEFG"), None);
    }

    #[test]
    fn custom_codes_ignore_case_hyphens_and_spaces_only() {
        assert_eq!(normalize_custom("spring-2027").as_deref(), Some("SPRING2027"));
        assert_eq!(normalize_custom("SPRING 2027").as_deref(), Some("SPRING2027"));
        assert_eq!(normalize_custom("abc"), None);
        assert_eq!(normalize_custom("hello!"), None);
        assert_eq!(normalize_custom(&"A".repeat(33)), None);
    }

    #[test]
    fn hashes_depend_on_key_kind_and_code() {
        let a = hash(b"k1", "serial", "ABC");
        assert_eq!(a, hash(b"k1", "serial", "ABC"));
        assert_ne!(a, hash(b"k2", "serial", "ABC"));
        assert_ne!(a, hash(b"k1", "custom", "ABC"));
        assert_ne!(a, hash(b"k1", "serial", "ABD"));
        assert_eq!(a.len(), 32);
        assert_eq!(hint("ABCDEFGHJKMN"), "JKMN");
    }

    #[test]
    fn five_bit_reads_the_stream_most_significant_first() {
        assert_eq!(five_bit(&[0b1111_1000, 0b0000_0111, 0b1100_0000], 4), vec![0b11111, 0b00000, 0b00011, 0b11100]);
    }
}
