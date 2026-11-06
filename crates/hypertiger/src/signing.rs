//! Keccak, EIP-712 and secp256k1: the two signing schemes the exchange accepts.
//!
//! L1 actions are hashed from msgpack and signed as a phantom `Agent`, so an action is bound to its
//! nonce, its vault and its expiry. User actions are signed directly as typed data, so a wallet can
//! display them. The two share nothing past the final digest, which is why the docs list "not
//! realizing there are two schemes" as the first way to get this wrong.

use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, SigningKey, VerifyingKey};
use sha3::{Digest, Keccak256};

use crate::sink::{KeccakTarget, MsgpackSink, Walk};
use crate::types::{Address, Network};

/// The chain id the phantom agent is signed under. It is not a real chain; it never changes.
pub const L1_CHAIN_ID: u64 = 1337;
/// The chain id user actions are signed under. The exchange accepts any and echoes it back; the
/// SDKs use Arbitrum Sepolia's, and `hyperliquidChain` is what prevents replay across networks.
pub const USER_CHAIN_ID: u64 = 0x66eee;
/// `USER_CHAIN_ID` as the action's `signatureChainId` field.
pub const USER_CHAIN_ID_HEX: &str = "0x66eee";

const DOMAIN_TYPE: &[u8] =
    b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
const AGENT_TYPE: &[u8] = b"Agent(string source,bytes32 connectionId)";

/// Keccak-256 of one byte string.
#[must_use]
pub fn keccak(bytes: &[u8]) -> [u8; 32] {
    Keccak256::digest(bytes).into()
}

/// The 32-byte big-endian word EIP-712 encodes every scalar as.
#[must_use]
pub fn word_u64(value: u64) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&value.to_be_bytes());
    word
}

/// An address left-padded to a word.
#[must_use]
pub fn word_address(address: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(&address.0);
    word
}

fn domain_separator(name: &[u8], chain_id: u64) -> [u8; 32] {
    assert_ne!(name, []);
    assert!(chain_id > 0);
    let mut hasher = Keccak256::new();
    hasher.update(keccak(DOMAIN_TYPE));
    hasher.update(keccak(name));
    hasher.update(keccak(b"1"));
    hasher.update(word_u64(chain_id));
    hasher.update(word_address(Address::ZERO));
    hasher.finalize().into()
}

/// The EIP-191 frame around a typed-data struct.
#[must_use]
pub fn typed_digest(domain: &[u8; 32], struct_hash: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update([0x19, 0x01]);
    hasher.update(domain);
    hasher.update(struct_hash);
    hasher.finalize().into()
}

/// The domain every user action is signed under.
#[must_use]
pub fn user_domain() -> [u8; 32] {
    domain_separator(b"HyperliquidSignTransaction", USER_CHAIN_ID)
}

/// The hash that binds an L1 action to its nonce, vault and expiry: the phantom agent's
/// `connectionId`.
///
/// `expires_after_ms` appends a zero byte before the timestamp while `vault` appends a one before
/// the address. That asymmetry is the exchange's, and the vectors pin it.
#[must_use]
pub fn l1_action_hash<W: Walk>(
    action: &W,
    nonce: u64,
    vault: Option<Address>,
    expires_after_ms: Option<u64>,
) -> [u8; 32] {
    assert!(expires_after_ms.is_none() || nonce > 0, "an expiring action needs a real nonce");
    let mut sink = MsgpackSink::new(KeccakTarget::new());
    action.walk(&mut sink);
    let mut target = sink.finish();
    target_write(&mut target, &nonce.to_be_bytes());
    match vault {
        None => target_write(&mut target, &[0x00]),
        Some(address) => {
            target_write(&mut target, &[0x01]);
            target_write(&mut target, &address.0);
        }
    }
    if let Some(expires_after_ms) = expires_after_ms {
        target_write(&mut target, &[0x00]);
        target_write(&mut target, &expires_after_ms.to_be_bytes());
    }
    target.finish()
}

fn target_write(target: &mut KeccakTarget, bytes: &[u8]) {
    crate::sink::ByteTarget::write(target, bytes);
}

/// The digest actually signed for an L1 action.
#[must_use]
pub fn l1_digest(action_hash: &[u8; 32], network: Network) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(keccak(AGENT_TYPE));
    hasher.update(keccak(network.agent_source().as_bytes()));
    hasher.update(action_hash);
    let struct_hash: [u8; 32] = hasher.finalize().into();
    typed_digest(&domain_separator(b"Exchange", L1_CHAIN_ID), &struct_hash)
}

/// A recoverable secp256k1 signature, with `v` as the exchange wants it: 27 or 28.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature {
    pub r: [u8; 32],
    pub s: [u8; 32],
    pub v: u8,
}

impl core::fmt::Debug for Signature {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "Signature {{ r: {}, s: {}, v: {} }}",
            ScalarHex::new(&self.r).as_str(),
            ScalarHex::new(&self.s).as_str(),
            self.v
        )
    }
}

/// `0x` and up to 64 digits.
pub const SCALAR_HEX_SIZE_MAX: usize = 66;

/// A signature scalar as the wire carries it: lowercase hex with leading zeros stripped, matching
/// the reference SDK's `to_hex(int)`. This form matters inside `multiSig`, where the inner
/// signatures are themselves hashed as strings.
#[derive(Clone, Copy)]
pub struct ScalarHex {
    text: [u8; SCALAR_HEX_SIZE_MAX],
    size: u8,
}

impl core::fmt::Debug for ScalarHex {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl ScalarHex {
    #[must_use]
    pub fn new(scalar: &[u8; 32]) -> ScalarHex {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = [0u8; SCALAR_HEX_SIZE_MAX];
        text[0] = b'0';
        text[1] = b'x';
        let mut size = 2usize;
        for byte in scalar {
            for nibble in [byte >> 4, byte & 0x0f] {
                if size > 2 || nibble != 0 {
                    text[size] = HEX[usize::from(nibble)];
                    size += 1;
                }
            }
        }
        if size == 2 {
            text[2] = b'0';
            size = 3;
        }
        assert!(size <= SCALAR_HEX_SIZE_MAX);
        ScalarHex { text, size: size as u8 }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        let Ok(text) = core::str::from_utf8(&self.text[..usize::from(self.size)]) else {
            panic!("scalar hex is ASCII");
        };
        text
    }
}

/// Anything that can sign a digest: a key held here, or a hardware module behind this trait.
pub trait Signer {
    fn address(&self) -> Address;
    fn sign_digest(&self, digest: &[u8; 32]) -> Signature;
}

/// A private key held in memory. It has no `Display`, and its `Debug` prints only the address, so
/// the key cannot reach a log by formatting.
pub struct Wallet {
    key: SigningKey,
    address: Address,
}

impl core::fmt::Debug for Wallet {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "Wallet({})", self.address)
    }
}

impl Wallet {
    /// `None` when the bytes are not a valid scalar: zero, or past the curve order.
    #[must_use]
    pub fn from_bytes(secret: &[u8; 32]) -> Option<Wallet> {
        let key = SigningKey::from_slice(secret).ok()?;
        let address = address_of(key.verifying_key());
        Some(Wallet { key, address })
    }
}

impl Signer for Wallet {
    fn address(&self) -> Address {
        self.address
    }

    /// Deterministic (RFC 6979) and low-s, as the reference SDK's signer is. The signature is
    /// recovered before it leaves: a bit flipped in memory becomes a crash, not a rejected order.
    fn sign_digest(&self, digest: &[u8; 32]) -> Signature {
        let Ok((signature, recovery)) = self.key.sign_prehash_recoverable(digest) else {
            panic!("RFC 6979 signing fails only with negligible probability");
        };
        let (r, s) = signature.split_bytes();
        let result = Signature { r: r.into(), s: s.into(), v: 27 + recovery.to_byte() };
        assert!(result.v == 27 || result.v == 28);
        assert_eq!(recover(digest, &result), Some(self.address));
        result
    }
}

fn address_of(key: &VerifyingKey) -> Address {
    let point = key.to_encoded_point(false);
    let uncompressed = point.as_bytes();
    assert_eq!(uncompressed.len(), 65);
    assert_eq!(uncompressed[0], 0x04);
    let hash = keccak(&uncompressed[1..]);
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..]);
    Address(address)
}

/// The address a signature recovers to, or `None` if it recovers to nothing.
///
/// The docs' fifth trap applies: this proves the signature matches this digest, not that the digest
/// matches what the exchange will compute. The vectors and the conformance run prove that.
#[must_use]
pub fn recover(digest: &[u8; 32], signature: &Signature) -> Option<Address> {
    let parity = signature.v.checked_sub(27)?;
    let recovery = RecoveryId::from_byte(parity)?;
    let ecdsa = EcdsaSignature::from_scalars(signature.r, signature.s).ok()?;
    let key = VerifyingKey::recover_from_prehash(digest, &ecdsa, recovery).ok()?;
    Some(address_of(&key))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x67, 0x89,
        0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x67, 0x89,
        0x01, 0x23,
    ];

    #[test]
    fn keccak_matches_the_published_empty_digest() {
        let empty = keccak(b"");
        assert_eq!(empty[..4], [0xc5, 0xd2, 0x46, 0x01]);
    }

    #[test]
    fn scalar_hex_strips_leading_zeros_only() {
        let mut scalar = [0u8; 32];
        assert_eq!(ScalarHex::new(&scalar).as_str(), "0x0");
        scalar[31] = 0x0a;
        assert_eq!(ScalarHex::new(&scalar).as_str(), "0xa");
        scalar[0] = 0x0f;
        assert_eq!(ScalarHex::new(&scalar).as_str().len(), 2 + 63);
        scalar[0] = 0xf0;
        assert_eq!(ScalarHex::new(&scalar).as_str().len(), 2 + 64);
    }

    #[test]
    fn wallet_refuses_invalid_scalars_and_hides_its_key() {
        assert!(Wallet::from_bytes(&[0u8; 32]).is_none());
        assert!(Wallet::from_bytes(&[0xff; 32]).is_none());
        let wallet = Wallet::from_bytes(&KEY).unwrap();
        let printed = std::format!("{wallet:?}");
        assert!(printed.contains(wallet.address().to_hex().as_str()));
        assert!(!printed.contains("0123456789012345"));
    }

    #[test]
    fn recover_inverts_signing_and_rejects_tampering() {
        let wallet = Wallet::from_bytes(&KEY).unwrap();
        let digest = keccak(b"a digest");
        let signature = wallet.sign_digest(&digest);
        assert_eq!(recover(&digest, &signature), Some(wallet.address()));
        assert_ne!(recover(&keccak(b"another"), &signature), Some(wallet.address()));

        let mut flipped = signature;
        flipped.v = if signature.v == 27 { 28 } else { 27 };
        assert_ne!(recover(&digest, &flipped), Some(wallet.address()));
        let mut out_of_range = signature;
        out_of_range.v = 26;
        assert_eq!(recover(&digest, &out_of_range), None);
        out_of_range.v = 31;
        assert_eq!(recover(&digest, &out_of_range), None);
        let mut zero_r = signature;
        zero_r.r = [0; 32];
        assert_eq!(recover(&digest, &zero_r), None);
    }

    #[test]
    fn the_networks_produce_different_digests() {
        let hash = keccak(b"action");
        assert_ne!(l1_digest(&hash, Network::Mainnet), l1_digest(&hash, Network::Testnet));
    }
}
