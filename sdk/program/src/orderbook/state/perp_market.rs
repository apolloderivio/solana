use super::{new_node_key, Side};
use crate::pubkey::Pubkey;
use borsh::{BorshDeserialize, BorshSerialize};
use derivative::Derivative;
use static_assertions::const_assert_eq;
use std::mem::size_of;

pub type PerpMarketIndex = u16;
pub type TokenIndex = u16;

#[repr(C)]
#[derive(Debug, Derivative, Copy, Clone, BorshSerialize, BorshDeserialize)]
#[borsh(crate = "borsh")]
pub struct PerpMarket {
    // ABI: Clients rely on this being at offset 8
    pub group: Pubkey,

    /// Token index that settlements happen in.
    ///
    /// Currently required to be 0, USDC. In the future settlement
    /// may be allowed to happen in other tokens.
    pub settle_token_index: TokenIndex,

    /// Index of this perp market. Other data, like the MangoAccount's PerpPosition
    /// reference this market via this index. Unique for this group's perp markets.
    pub perp_market_index: PerpMarketIndex,

    /// Is this market covered by the group insurance fund?
    pub group_insurance_fund: u8,

    /// Number of decimals used for the base token.
    ///
    /// Used to convert the oracle's price into a native/native price.
    pub base_decimals: u8,

    /// Name. Trailing zero bytes are ignored.
    #[derivative(Debug(format_with = "util::format_zero_terminated_utf8_bytes"))]
    pub name: [u8; 16],

    /// Address of the BookSide account for bids
    pub bids: Pubkey,
    /// Address of the BookSide account for asks
    pub asks: Pubkey,
    /// Address of the EventQueue account
    pub event_queue: Pubkey,

    /// Number of quote native in a quote lot. Must be a power of 10.
    ///
    /// Primarily useful for increasing the tick size on the market: A lot price
    /// of 1 becomes a native price of quote_lot_size/base_lot_size becomes a
    /// ui price of quote_lot_size*base_decimals/base_lot_size/quote_decimals.
    pub quote_lot_size: i64,

    /// Number of base native in a base lot. Must be a power of 10.
    ///
    /// Example: If base decimals for the underlying asset is 6, base lot size
    /// is 100 and and base position lots is 10_000 then base position native is
    /// 1_000_000 and base position ui is 1.
    pub base_lot_size: i64,

    /// Number of base lots currently active in the market. Always >= 0.
    ///
    /// Since this counts positive base lots and negative base lots, the more relevant
    /// number of open base lot pairs is half this value.
    pub open_interest: i64,

    /// Total number of orders seen
    pub seq_num: u64,

    /// Timestamp in seconds that the market was registered at.
    pub registration_time: u64,

    /// Fee (in quote native) to charge for ioc orders
    pub fee_penalty: f32,

    /// If true, users may no longer increase their market exposure. Only actions
    /// that reduce their position are still allowed.
    pub reduce_only: u8,
    pub force_close: u8,
}
unsafe impl bytemuck::Pod for PerpMarket {}
unsafe impl bytemuck::Zeroable for PerpMarket {}

const_assert_eq!(size_of::<PerpMarket>() % 8, 0);

impl PerpMarket {
    pub fn name(&self) -> &str {
        std::str::from_utf8(&self.name)
            .unwrap()
            .trim_matches(char::from(0))
    }

    pub fn is_reduce_only(&self) -> bool {
        self.reduce_only == 1
    }

    pub fn is_force_close(&self) -> bool {
        self.force_close == 1
    }

    pub fn elligible_for_group_insurance_fund(&self) -> bool {
        self.group_insurance_fund == 1
    }

    pub fn set_elligible_for_group_insurance_fund(&mut self, v: bool) {
        self.group_insurance_fund = u8::from(v);
    }

    pub fn gen_order_id(&mut self, side: Side, price_data: u64) -> u128 {
        self.seq_num += 1;
        new_node_key(side, price_data, self.seq_num)
    }

    /// Creates default market for tests
    pub fn default_for_tests() -> PerpMarket {
        PerpMarket {
            group: Pubkey::new_unique(),
            settle_token_index: 0,
            perp_market_index: 0,
            group_insurance_fund: 0,
            base_decimals: 0,
            name: Default::default(),
            bids: Pubkey::new_unique(),
            asks: Pubkey::new_unique(),
            event_queue: Pubkey::new_unique(),
            quote_lot_size: 1,
            base_lot_size: 1,
            open_interest: 0,
            seq_num: 0,
            registration_time: 0,
            fee_penalty: 0.0,
            reduce_only: 0,
            force_close: 0,
        }
    }
}
