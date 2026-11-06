//! Assets and the exchange's price and size grid.
//!
//! An order off the grid is rejected by the exchange after a round trip that costs rate limit.
//! Checking here costs a few divisions, and rounding is explicit about its direction because the
//! caller's side decides which way is safe: a buy must not round up past what it meant to pay.

use crate::decimal::{Decimal, DecimalError, Px, Sz, WIRE_ONE};

/// The first spot asset id. Below it are the default dex's perps.
pub const ASSET_SPOT_BASE: u32 = 10_000;
/// The first builder-deployed perp asset id.
pub const ASSET_BUILDER_BASE: u32 = 100_000;
/// How many asset ids each builder dex owns.
pub const ASSET_BUILDER_DEX_SIZE: u32 = 10_000;
/// The first outcome asset id.
pub const ASSET_OUTCOME_BASE: u32 = 100_000_000;

const _: () = assert!(ASSET_SPOT_BASE < ASSET_BUILDER_BASE);
const _: () = assert!(ASSET_BUILDER_BASE < ASSET_OUTCOME_BASE);
const _: () = assert!(ASSET_BUILDER_DEX_SIZE == ASSET_SPOT_BASE);

/// Decimal places allowed on a perp price, before `szDecimals` is taken off.
pub const PERP_DECIMALS_MAX: u8 = 6;
/// Decimal places allowed on a spot price, before `szDecimals` is taken off.
pub const SPOT_DECIMALS_MAX: u8 = 8;
/// Significant figures allowed on a non-integer price.
pub const PX_SIGNIFICANT_MAX: u32 = 5;

const _: () = assert!(PERP_DECIMALS_MAX < SPOT_DECIMALS_MAX);
const _: () = assert!(SPOT_DECIMALS_MAX as u32 == crate::decimal::WIRE_DECIMALS);
const _: () = assert!(PX_SIGNIFICANT_MAX > 0);
const _: () = assert!(NOTIONAL_MIN_QUOTE > 0);

/// The exchange's minimum order value in whole quote units, applied to both perps and spot.
pub const NOTIONAL_MIN_QUOTE: i128 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Perp,
    Spot,
    BuilderPerp,
    Outcome,
}

/// The integer the exchange trades an asset under. Mainnet and testnet assign different ones, so
/// they come from `meta` at startup and never from a constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Asset(u32);

impl Asset {
    #[must_use]
    pub fn perp(index: u32) -> Asset {
        assert!(index < ASSET_SPOT_BASE);
        Asset(index)
    }

    #[must_use]
    pub fn spot(index: u32) -> Asset {
        assert!(index < ASSET_BUILDER_BASE - ASSET_SPOT_BASE);
        Asset(ASSET_SPOT_BASE + index)
    }

    #[must_use]
    pub fn builder_perp(dex_index: u32, index: u32) -> Asset {
        assert!(index < ASSET_BUILDER_DEX_SIZE);
        let dexes_max = (ASSET_OUTCOME_BASE - ASSET_BUILDER_BASE) / ASSET_BUILDER_DEX_SIZE;
        assert!(dex_index < dexes_max);
        Asset(ASSET_BUILDER_BASE + dex_index * ASSET_BUILDER_DEX_SIZE + index)
    }

    /// `encoding = 10 * outcome + side`, and only sides 0 and 1 exist.
    #[must_use]
    pub fn outcome(outcome: u32, side: u32) -> Asset {
        assert!(side <= 1);
        assert!(outcome < (u32::MAX - ASSET_OUTCOME_BASE) / 10);
        Asset(ASSET_OUTCOME_BASE + 10 * outcome + side)
    }

    /// Takes an id off the wire, where its range has not yet been checked by a constructor.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Asset {
        Asset(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn kind(self) -> AssetKind {
        if self.0 < ASSET_SPOT_BASE {
            AssetKind::Perp
        } else if self.0 < ASSET_BUILDER_BASE {
            AssetKind::Spot
        } else if self.0 < ASSET_OUTCOME_BASE {
            AssetKind::BuilderPerp
        } else {
            AssetKind::Outcome
        }
    }
}

/// Which way a value may move to reach the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Round {
    Down,
    Up,
}

/// Why a value is off the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridError {
    Zero,
    PxOffTick,
    SzOffLot,
    NotionalBelowMinimum,
    Overflow,
    Decimal(DecimalError),
}

/// An asset's grid. `sz_decimals` comes from the `meta` response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Market {
    asset: Asset,
    sz_decimals: u8,
}

impl Market {
    /// `None` when `sz_decimals` leaves the asset's price no decimal budget to subtract from.
    #[must_use]
    pub fn new(asset: Asset, sz_decimals: u8) -> Option<Market> {
        let market = Market { asset, sz_decimals };
        assert!(market.decimals_max() <= SPOT_DECIMALS_MAX);
        if sz_decimals <= market.decimals_max() { Some(market) } else { None }
    }

    #[must_use]
    pub const fn asset(self) -> Asset {
        assert!(self.sz_decimals <= self.decimals_max());
        self.asset
    }

    #[must_use]
    pub const fn sz_decimals(self) -> u8 {
        assert!(self.sz_decimals <= SPOT_DECIMALS_MAX);
        self.sz_decimals
    }

    const fn decimals_max(self) -> u8 {
        match self.asset.kind() {
            AssetKind::Perp | AssetKind::BuilderPerp => PERP_DECIMALS_MAX,
            // Outcomes "share most implementation details with spot" per the docs.
            AssetKind::Spot | AssetKind::Outcome => SPOT_DECIMALS_MAX,
        }
    }

    /// The tick at this price level, in raw `Px` units.
    fn px_tick(self, raw: u64) -> u64 {
        assert!(raw > 0);
        let decimals_budget = u32::from(self.decimals_max() - self.sz_decimals);
        assert!(decimals_budget <= crate::decimal::WIRE_DECIMALS);
        let tick_decimals = 10u64.pow(crate::decimal::WIRE_DECIMALS - decimals_budget);

        let digits_count = raw.ilog10() + 1;
        let tick_significant = if digits_count > PX_SIGNIFICANT_MAX {
            10u64.pow(digits_count - PX_SIGNIFICANT_MAX)
        } else {
            1
        };
        assert!(tick_significant > 0);
        // Integer prices are always allowed, however many figures they have, so no tick exceeds 1.
        tick_decimals.max(tick_significant).min(WIRE_ONE)
    }

    /// Whether the exchange's grid allows this price.
    pub fn px_check(self, px: Px) -> Result<(), GridError> {
        if px.raw() == 0 {
            return Err(GridError::Zero);
        }
        let tick = self.px_tick(px.raw());
        assert!(tick > 0);
        assert!(tick <= WIRE_ONE, "integer prices are always allowed");
        if px.raw() % tick == 0 { Ok(()) } else { Err(GridError::PxOffTick) }
    }

    /// The nearest price on the grid in the given direction. `None` only at the top of the range.
    #[must_use]
    pub fn px_round(self, px: Px, round: Round) -> Option<Px> {
        if px.raw() == 0 {
            return None;
        }
        let tick = self.px_tick(px.raw());
        let below = px.raw() - px.raw() % tick;
        assert!(below <= px.raw());
        assert!(below % tick == 0);
        let rounded = match round {
            Round::Down if below > 0 => below,
            Round::Down => tick,
            Round::Up if below == px.raw() => below,
            Round::Up => below.checked_add(tick)?,
        };
        let result = Px::from_raw(rounded);
        assert!(self.px_check(result).is_ok());
        Some(result)
    }

    /// Whether the exchange's lot allows this size.
    pub fn sz_check(self, sz: Sz) -> Result<(), GridError> {
        if sz.raw() == 0 {
            return Err(GridError::Zero);
        }
        let lot = self.sz_lot();
        assert!(lot > 0);
        if sz.raw() % lot == 0 { Ok(()) } else { Err(GridError::SzOffLot) }
    }

    /// The largest whole number of lots at or below `sz`. Sizes only ever round down: a rounded-up
    /// size is a risk the caller did not ask to take.
    #[must_use]
    pub fn sz_round_down(self, sz: Sz) -> Sz {
        let lot = self.sz_lot();
        let result = Sz::from_raw(sz.raw() - sz.raw() % lot);
        assert!(result <= sz);
        assert!(result.raw() % lot == 0);
        result
    }

    fn sz_lot(self) -> u128 {
        assert!(self.sz_decimals <= self.decimals_max());
        assert!(u32::from(self.sz_decimals) <= crate::decimal::WIRE_DECIMALS);
        10u128.pow(crate::decimal::WIRE_DECIMALS - u32::from(self.sz_decimals))
    }

    /// The whole pre-flight: grid and minimum value. Reduce-only orders are exempt from the
    /// minimum on the exchange, so the caller says which applies.
    pub fn order_check(self, px: Px, sz: Sz, reduce_only: bool) -> Result<(), GridError> {
        self.px_check(px)?;
        self.sz_check(sz)?;
        assert!(px.raw() > 0);
        assert!(sz.raw() > 0);
        if reduce_only {
            return Ok(());
        }
        let notional = sz.notional_floor(px).ok_or(GridError::Overflow)?;
        let minimum = Decimal::from_raw(NOTIONAL_MIN_QUOTE * crate::decimal::DECIMAL_ONE);
        if notional >= minimum { Ok(()) } else { Err(GridError::NotionalBelowMinimum) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(text: &str) -> Px {
        Px::parse(text.as_bytes()).unwrap()
    }

    fn sz(text: &str) -> Sz {
        Sz::parse(text.as_bytes()).unwrap()
    }

    fn perp(sz_decimals: u8) -> Market {
        Market::new(Asset::perp(0), sz_decimals).unwrap()
    }

    fn spot(sz_decimals: u8) -> Market {
        Market::new(Asset::spot(0), sz_decimals).unwrap()
    }

    /// The docs' own worked examples, positive and negative.
    #[test]
    fn documented_examples() {
        assert_eq!(perp(0).px_check(px("1234.5")), Ok(()));
        assert_eq!(perp(0).px_check(px("1234.56")), Err(GridError::PxOffTick));
        assert_eq!(perp(0).px_check(px("0.001234")), Ok(()));
        assert_eq!(perp(0).px_check(px("0.0012345")), Err(GridError::PxOffTick));
        assert_eq!(perp(1).px_check(px("0.01234")), Ok(()));
        assert_eq!(perp(1).px_check(px("0.012345")), Err(GridError::PxOffTick));
        assert_eq!(perp(0).px_check(px("123456")), Ok(()));
        assert_eq!(perp(0).px_check(px("12345.6")), Err(GridError::PxOffTick));
        assert_eq!(spot(0).px_check(px("0.0001234")), Ok(()));
        assert_eq!(spot(1).px_check(px("0.0001234")), Ok(()));
        assert_eq!(spot(2).px_check(px("0.0001234")), Err(GridError::PxOffTick));
        assert_eq!(perp(3).sz_check(sz("1.001")), Ok(()));
        assert_eq!(perp(3).sz_check(sz("1.0001")), Err(GridError::SzOffLot));
    }

    #[test]
    fn zero_is_never_on_the_grid() {
        assert_eq!(perp(0).px_check(Px::ZERO), Err(GridError::Zero));
        assert_eq!(perp(0).sz_check(Sz::ZERO), Err(GridError::Zero));
        assert_eq!(perp(0).px_round(Px::ZERO, Round::Up), None);
    }

    #[test]
    fn rounding_moves_only_in_the_direction_asked() {
        let market = perp(0);
        assert_eq!(market.px_round(px("1234.56"), Round::Down), Some(px("1234.5")));
        assert_eq!(market.px_round(px("1234.56"), Round::Up), Some(px("1234.6")));
        assert_eq!(market.px_round(px("1234.5"), Round::Up), Some(px("1234.5")));
        assert_eq!(market.px_round(px("1234.5"), Round::Down), Some(px("1234.5")));
        assert_eq!(market.px_round(px("9999.95"), Round::Up), Some(px("10000")));
        assert_eq!(market.px_round(px("99999.5"), Round::Up), Some(px("100000")));
        assert_eq!(market.px_round(px("100000.5"), Round::Down), Some(px("100000")));
        assert_eq!(market.px_round(px("0.0000001"), Round::Up), Some(px("0.000001")));
        assert_eq!(market.px_round(px("0.0000001"), Round::Down), Some(px("0.000001")));
    }

    /// Rounded values land on the grid, never cross the input in the wrong direction, and a value
    /// already on the grid is a fixed point. Sweeps every raw value in several price decades.
    #[test]
    fn rounding_properties_over_dense_bands() {
        for market in [perp(0), perp(3), perp(5), spot(0), spot(4), spot(8)] {
            for base in [1u64, 1_000, 1_000_000, 99_990_000, 100_000_000, 999_999_000_000] {
                let mut raw = base;
                while raw < base + 3_000 {
                    let input = Px::from_raw(raw);
                    let down = market.px_round(input, Round::Down).unwrap();
                    let up = market.px_round(input, Round::Up).unwrap();
                    assert!(up >= input);
                    assert_eq!(market.px_check(down), Ok(()));
                    assert_eq!(market.px_check(up), Ok(()));
                    if down.raw() <= raw {
                        assert!(raw - down.raw() <= market.px_tick(raw));
                    }
                    if market.px_check(input).is_ok() {
                        assert_eq!(down, input);
                        assert_eq!(up, input);
                    }
                    raw += 1;
                }
            }
        }
    }

    #[test]
    fn sizes_round_down_only() {
        assert_eq!(perp(3).sz_round_down(sz("1.0019")), sz("1.001"));
        assert_eq!(perp(3).sz_round_down(sz("1.001")), sz("1.001"));
        assert_eq!(perp(0).sz_round_down(sz("0.9")), Sz::ZERO);
        assert_eq!(spot(8).sz_round_down(sz("0.00000001")), sz("0.00000001"));
    }

    #[test]
    fn minimum_notional_applies_unless_reduce_only() {
        let market = perp(2);
        assert_eq!(market.order_check(px("100"), sz("0.1"), false), Ok(()));
        assert_eq!(
            market.order_check(px("100"), sz("0.09"), false),
            Err(GridError::NotionalBelowMinimum)
        );
        assert_eq!(market.order_check(px("100"), sz("0.09"), true), Ok(()));
        assert_eq!(market.order_check(px("100.123"), sz("1"), true), Err(GridError::PxOffTick));
        assert_eq!(market.order_check(px("100"), sz("0.001"), true), Err(GridError::SzOffLot));
    }

    #[test]
    fn asset_ids_follow_the_documented_encoding() {
        assert_eq!(Asset::perp(0).raw(), 0);
        assert_eq!(Asset::spot(107).raw(), 10_107);
        assert_eq!(Asset::builder_perp(1, 0).raw(), 110_000);
        assert_eq!(Asset::outcome(1, 0).raw(), 100_000_010);
        assert_eq!(Asset::perp(9_999).kind(), AssetKind::Perp);
        assert_eq!(Asset::spot(0).kind(), AssetKind::Spot);
        assert_eq!(Asset::spot(89_999).kind(), AssetKind::Spot);
        assert_eq!(Asset::builder_perp(0, 0).kind(), AssetKind::BuilderPerp);
        assert_eq!(Asset::outcome(0, 1).kind(), AssetKind::Outcome);
    }

    #[test]
    #[should_panic(expected = "side <= 1")]
    fn outcome_side_outside_the_binary_is_a_bug() {
        let _ = Asset::outcome(1, 2);
    }

    #[test]
    fn sz_decimals_past_the_budget_are_refused() {
        assert!(Market::new(Asset::perp(0), 7).is_none());
        assert!(Market::new(Asset::perp(0), 6).is_some());
        assert!(Market::new(Asset::spot(0), 9).is_none());
        assert!(Market::new(Asset::spot(0), 8).is_some());
    }
}
