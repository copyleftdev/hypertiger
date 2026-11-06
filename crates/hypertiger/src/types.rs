//! The small nouns: addresses, client order ids, sides, networks and inline coin names.

use core::fmt;

/// Which chain an action is signed for. A value with no default, so that nothing reaches mainnet
/// by omission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Testnet,
}

impl Network {
    /// The phantom agent's `source`, which is what separates the two chains under L1 signing.
    #[must_use]
    pub const fn agent_source(self) -> &'static str {
        match self {
            Network::Mainnet => "a",
            Network::Testnet => "b",
        }
    }

    /// The `hyperliquidChain` field, which separates the two chains under user signing.
    #[must_use]
    pub const fn chain_name(self) -> &'static str {
        match self {
            Network::Mainnet => "Mainnet",
            Network::Testnet => "Testnet",
        }
    }
}

const _: () = assert!(Network::Mainnet.agent_source().len() == 1);
const _: () = assert!(Network::Testnet.agent_source().len() == 1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    #[must_use]
    pub const fn is_buy(self) -> bool {
        matches!(self, Side::Buy)
    }

    /// Parses the feed's `B` and `A`.
    #[must_use]
    pub fn parse(text: &[u8]) -> Option<Side> {
        match text {
            b"B" => Some(Side::Buy),
            b"A" => Some(Side::Sell),
            _ => None,
        }
    }
}

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decodes `0x`-prefixed or bare hex into `target`, which must match in length exactly.
fn hex_decode(text: &[u8], target: &mut [u8]) -> Option<()> {
    let digits = text.strip_prefix(b"0x").unwrap_or(text);
    if digits.len() == target.len() * 2 {
        let mut index = 0usize;
        while index < target.len() {
            let high = hex_value(digits[index * 2])?;
            let low = hex_value(digits[index * 2 + 1])?;
            target[index] = (high << 4) | low;
            index += 1;
        }
        Some(())
    } else {
        None
    }
}

fn hex_encode(source: &[u8], target: &mut [u8]) {
    assert_eq!(target.len(), 2 + source.len() * 2);
    target[0] = b'0';
    target[1] = b'x';
    let mut index = 0usize;
    while index < source.len() {
        target[2 + index * 2] = HEX_LOWER[usize::from(source[index] >> 4)];
        target[3 + index * 2] = HEX_LOWER[usize::from(source[index] & 0x0f)];
        index += 1;
    }
}

fn ascii_str(bytes: &[u8]) -> &str {
    let Ok(text) = core::str::from_utf8(bytes) else {
        panic!("hex output is ASCII");
    };
    text
}

/// A 20-byte address, always rendered lowercase. The docs warn that uppercase addresses sign one
/// thing and verify another; holding bytes rather than text makes that unrepresentable.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Address(pub [u8; 20]);

/// `0x` and 40 digits.
pub const ADDRESS_HEX_SIZE: usize = 42;

impl Address {
    pub const ZERO: Address = Address([0; 20]);

    #[must_use]
    pub fn parse(text: &[u8]) -> Option<Address> {
        let mut bytes = [0u8; 20];
        hex_decode(text, &mut bytes)?;
        Some(Address(bytes))
    }

    #[must_use]
    pub fn to_hex(self) -> AddressHex {
        let mut text = [0u8; ADDRESS_HEX_SIZE];
        hex_encode(&self.0, &mut text);
        AddressHex(text)
    }
}

/// An address rendered as lowercase `0x` hex, inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressHex([u8; ADDRESS_HEX_SIZE]);

impl AddressHex {
    #[must_use]
    pub fn as_str(&self) -> &str {
        ascii_str(&self.0)
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.to_hex().as_str())
    }
}

impl fmt::Display for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.to_hex().as_str())
    }
}

/// `0x` and 32 digits.
pub const CLOID_HEX_SIZE: usize = 34;

/// A 128-bit client order id, chosen by the client: the same way TigerBeetle's clients choose
/// transfer ids, and for the same reason. The client can name an order before knowing whether it
/// landed, and that name is what the ambiguity is later resolved by.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cloid(pub [u8; 16]);

impl Cloid {
    /// A cloid made of a session, which tells this process's orders from any other's, and a
    /// sequence, which orders them. Big-endian, so the hex reads as the two numbers side by side.
    #[must_use]
    pub fn from_parts(session: u64, sequence: u64) -> Cloid {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&session.to_be_bytes());
        bytes[8..].copy_from_slice(&sequence.to_be_bytes());
        Cloid(bytes)
    }

    #[must_use]
    pub fn session(self) -> u64 {
        let mut head = [0u8; 8];
        head.copy_from_slice(&self.0[..8]);
        u64::from_be_bytes(head)
    }

    #[must_use]
    pub fn sequence(self) -> u64 {
        let mut tail = [0u8; 8];
        tail.copy_from_slice(&self.0[8..]);
        u64::from_be_bytes(tail)
    }

    #[must_use]
    pub fn parse(text: &[u8]) -> Option<Cloid> {
        let mut bytes = [0u8; 16];
        hex_decode(text, &mut bytes)?;
        Some(Cloid(bytes))
    }

    #[must_use]
    pub fn to_hex(self) -> CloidHex {
        let mut text = [0u8; CLOID_HEX_SIZE];
        hex_encode(&self.0, &mut text);
        CloidHex(text)
    }
}

/// A cloid rendered as lowercase `0x` hex, inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloidHex([u8; CLOID_HEX_SIZE]);

impl CloidHex {
    #[must_use]
    pub fn as_str(&self) -> &str {
        ascii_str(&self.0)
    }
}

impl fmt::Debug for Cloid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.to_hex().as_str())
    }
}

/// The longest coin name held inline. The longest measured on mainnet was 13 (`xyz:ALUMINIUM`);
/// builder dexes prefix names, so the margin is deliberate.
pub const COIN_SIZE_MAX: usize = 31;

const _: () = assert!(COIN_SIZE_MAX < u8::MAX as usize);

/// A coin name, inline, so that a feed message can be retained without a heap.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Coin {
    bytes: [u8; COIN_SIZE_MAX],
    size: u8,
}

impl Coin {
    /// `None` when the name is empty, too long, or not ASCII. Exchange names are all ASCII, and
    /// refusing the rest keeps `as_str` infallible.
    #[must_use]
    pub fn parse(text: &[u8]) -> Option<Coin> {
        if text.is_empty() || text.len() > COIN_SIZE_MAX || !text.is_ascii() {
            return None;
        }
        let mut bytes = [0u8; COIN_SIZE_MAX];
        bytes[..text.len()].copy_from_slice(text);
        Some(Coin { bytes, size: text.len() as u8 })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        assert!(self.size > 0);
        assert!(usize::from(self.size) <= COIN_SIZE_MAX);
        ascii_str(&self.bytes[..usize::from(self.size)])
    }
}

impl fmt::Debug for Coin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Display for Coin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_renders_lowercase_whatever_it_was_given() {
        let address = Address::parse(b"0x5E9EE1089755C3435139848E47E6635505D5A13A").unwrap();
        assert_eq!(address.to_hex().as_str(), "0x5e9ee1089755c3435139848e47e6635505d5a13a");
        assert_eq!(Address::parse(address.to_hex().as_str().as_bytes()), Some(address));
    }

    #[test]
    fn address_refuses_wrong_lengths_and_digits() {
        assert_eq!(Address::parse(b"0x"), None);
        assert_eq!(Address::parse(b"0x5e9e"), None);
        assert_eq!(Address::parse(b"0x5e9ee1089755c3435139848e47e6635505d5a13g"), None);
        assert_eq!(Address::parse(b"0x5e9ee1089755c3435139848e47e6635505d5a13a00"), None);
        assert!(Address::parse(b"5e9ee1089755c3435139848e47e6635505d5a13a").is_some());
    }

    #[test]
    fn cloid_parts_round_trip() {
        let cloid = Cloid::from_parts(0x0102_0304_0506_0708, 0x1112_1314_1516_1718);
        assert_eq!(cloid.session(), 0x0102_0304_0506_0708);
        assert_eq!(cloid.sequence(), 0x1112_1314_1516_1718);
        assert_eq!(cloid.to_hex().as_str(), "0x01020304050607081112131415161718");
        assert_eq!(Cloid::parse(cloid.to_hex().as_str().as_bytes()), Some(cloid));
        assert_eq!(Cloid::parse(b"0x01"), None);
    }

    #[test]
    fn coin_holds_real_names_and_refuses_the_rest() {
        for name in ["BTC", "xyz:ALUMINIUM", "PURR/USDC", "@107", "#114910", "+10"] {
            assert_eq!(Coin::parse(name.as_bytes()).unwrap().as_str(), name);
        }
        assert!(Coin::parse(b"").is_none());
        assert!(Coin::parse(&[b'A'; COIN_SIZE_MAX + 1]).is_none());
        assert!(Coin::parse(&[0xc3, 0xa9]).is_none());
        assert_eq!(Coin::parse(&[b'A'; COIN_SIZE_MAX]).unwrap().as_str().len(), COIN_SIZE_MAX);
    }

    #[test]
    fn side_parses_the_feed_alphabet_only() {
        assert_eq!(Side::parse(b"B"), Some(Side::Buy));
        assert_eq!(Side::parse(b"A"), Some(Side::Sell));
        assert_eq!(Side::parse(b"S"), None);
        assert_eq!(Side::parse(b""), None);
    }

    #[test]
    fn networks_differ_where_signatures_depend_on_it() {
        assert_ne!(Network::Mainnet.agent_source(), Network::Testnet.agent_source());
        assert_ne!(Network::Mainnet.chain_name(), Network::Testnet.chain_name());
    }
}
