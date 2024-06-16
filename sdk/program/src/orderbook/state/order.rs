use super::order_type::{PostOrderType, SelfTradeBehavior, Side};
use crate::clock::Clock;
use crate::program_error::ProgramError;

/// Perp order parameters
pub struct Order {
    pub side: Side,

    /// Max base lots to buy/sell.
    pub max_base_lots: i64,

    /// Max quote lots to pay/receive (not taking fees into account).
    pub max_quote_lots: i64,

    /// Arbitrary user-controlled order id.
    pub client_order_id: u64,

    /// Reduce only
    pub reduce_only: bool,

    /// Number of seconds the order shall live, 0 meaning forever
    pub time_in_force: u16,

    /// Configure how matches with order of the same owner are handled
    pub self_trade_behavior: SelfTradeBehavior,

    /// Order type specific params
    pub params: OrderParams,
}

pub enum OrderParams {
    Market,
    ImmediateOrCancel {
        price_lots: i64,
    },
    Fixed {
        price_lots: i64,
        order_type: PostOrderType,
    },
}

impl Order {
    /// Convert an input expiry timestamp to a time_in_force value
    pub fn tif_from_expiry(expiry_timestamp: u64) -> Option<u16> {
        let clock = Clock::get()?;
        let now_ts: u64 = clock.unix_timestamp as u64;
        if expiry_timestamp != 0 {
            // If expiry is far in the future, clamp to u16::MAX seconds
            let tif = expiry_timestamp.saturating_sub(now_ts).min(u16::MAX.into());
            if tif == 0 {
                // If expiry is in the past, ignore the order
                return None;
            }
            Some(tif as u16)
        } else {
            // Never expire
            Some(0)
        }
    }

    /// Should this order be penalized with an extra fee?
    ///
    /// Some programs opportunistically call ioc orders, wasting lots of compute. This
    /// is intended to encourage people to be smarter about it.
    pub fn needs_penalty_fee(&self) -> bool {
        matches!(self.params, OrderParams::ImmediateOrCancel { .. })
    }

    /// Is this order required to be posted to the orderbook? It will fail if it would take.
    pub fn is_post_only(&self) -> bool {
        let order_type = match self.params {
            OrderParams::Fixed { order_type, .. } => order_type,
            _ => return false,
        };
        order_type == PostOrderType::PostOnly
    }

    /// Compute the price_lots this order is currently at, as well as the price_data that
    /// would be stored in its OrderTree node if the order is posted to the orderbook.
    pub fn price(&self) -> Result<(i64, u64), ProgramError> {
        let price_lots = match self.params {
            OrderParams::Market { .. } => market_order_limit_for_side(self.side),
            OrderParams::ImmediateOrCancel { price_lots, .. } => price_lots,
            OrderParams::Fixed { price_lots, .. } => price_lots,
        };
        Ok((price_lots, price_lots as u64))
    }
}

/// The implicit limit price to use for market orders
fn market_order_limit_for_side(side: Side) -> i64 {
    match side {
        Side::Bid => i64::MAX,
        Side::Ask => 1,
    }
}

/// The limit to use for PostOnlySlide orders: the tinyest bit better than
/// the best opposing order
fn post_only_slide_limit(side: Side, best_other_side: i64, limit: i64) -> i64 {
    match side {
        Side::Bid => limit.min(best_other_side - 1),
        Side::Ask => limit.max(best_other_side + 1),
    }
}
