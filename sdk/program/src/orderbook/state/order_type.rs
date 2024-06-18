use super::super::error::MangoError;
use crate::program_error::ProgramError;
use borsh::{BorshDeserialize, BorshSerialize};
use num_enum::{IntoPrimitive, TryFromPrimitive};

#[derive(
    Eq,
    PartialEq,
    Copy,
    Clone,
    TryFromPrimitive,
    IntoPrimitive,
    Debug,
    BorshDeserialize,
    BorshSerialize,
)]
#[borsh(crate = "borsh")]
#[repr(u8)]
pub enum PlaceOrderType {
    /// Take existing orders up to price, max_base_quantity and max_quote_quantity.
    /// If any base_quantity or quote_quantity remains, place an order on the book
    Limit,

    /// Take existing orders up to price, max_base_quantity and max_quote_quantity.
    /// Never place an order on the book.
    ImmediateOrCancel,

    /// Never take any existing orders, post the order on the book if possible.
    /// If existing orders can match with this order, do nothing.
    PostOnly,

    /// Ignore price and take orders up to max_base_quantity and max_quote_quantity.
    /// Never place an order on the book.
    ///
    /// Equivalent to ImmediateOrCancel with price=i64::MAX.
    Market,
}

impl PlaceOrderType {
    pub fn to_post_order_type(&self) -> Result<PostOrderType, ProgramError> {
        match *self {
            Self::Market => Err(MangoError::SomeError.into()),
            Self::ImmediateOrCancel => Err(MangoError::SomeError.into()),
            Self::Limit => Ok(PostOrderType::Limit),
            Self::PostOnly => Ok(PostOrderType::PostOnly),
        }
    }
}

#[derive(
    Eq,
    PartialEq,
    Copy,
    Clone,
    TryFromPrimitive,
    IntoPrimitive,
    Debug,
    BorshDeserialize,
    BorshSerialize,
)]
#[borsh(crate = "borsh")]
#[repr(u8)]
pub enum PostOrderType {
    /// Take existing orders up to price, max_base_quantity and max_quote_quantity.
    /// If any base_quantity or quote_quantity remains, place an order on the book
    Limit,

    /// Never take any existing orders, post the order on the book if possible.
    /// If existing orders can match with this order, do nothing.
    PostOnly,
}

#[derive(
    Eq,
    PartialEq,
    Copy,
    Clone,
    Default,
    TryFromPrimitive,
    IntoPrimitive,
    Debug,
    BorshDeserialize,
    BorshSerialize,
)]
#[borsh(crate = "borsh")]
#[repr(u8)]
/// Self trade behavior controls how taker orders interact with resting limit orders of the same account.
/// This setting has no influence on placing a resting or oracle pegged limit order that does not match
/// immediately, instead it's the responsibility of the user to correctly configure his taker orders.
pub enum SelfTradeBehavior {
    /// Both the maker and taker sides of the matched orders are decremented.
    /// This is equivalent to a normal order match, except for the fact that no fees are applied.
    #[default]
    DecrementTake,

    /// Cancels the maker side of the trade, the taker side gets matched with other maker's orders.
    CancelProvide,

    /// Cancels the whole transaction as soon as a self-matching scenario is encountered.
    AbortTransaction,
}

#[derive(
    Eq,
    PartialEq,
    Copy,
    Clone,
    TryFromPrimitive,
    IntoPrimitive,
    Debug,
    BorshDeserialize,
    BorshSerialize,
)]
#[borsh(crate = "borsh")]
#[repr(u8)]
pub enum Side {
    Bid,
    Ask,
}

impl Side {
    pub fn invert_side(self: &Side) -> Side {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }

    /// Is `lhs` is a better order for `side` than `rhs`?
    pub fn is_price_data_better(self: &Side, lhs: u64, rhs: u64) -> bool {
        match self {
            Side::Bid => lhs > rhs,
            Side::Ask => lhs < rhs,
        }
    }

    /// Is `lhs` is a better order for `side` than `rhs`?
    pub fn is_price_better(self: &Side, lhs: i64, rhs: i64) -> bool {
        match self {
            Side::Bid => lhs > rhs,
            Side::Ask => lhs < rhs,
        }
    }

    /// Is `price` acceptable for a `limit` order on `side`?
    pub fn is_price_within_limit(self: &Side, price: i64, limit: i64) -> bool {
        match self {
            Side::Bid => price <= limit,
            Side::Ask => price >= limit,
        }
    }
}

/// SideAndOrderTree is a storage optimization, so we don't need two bytes for the data
#[derive(
    Eq,
    PartialEq,
    Copy,
    Clone,
    TryFromPrimitive,
    IntoPrimitive,
    Debug,
    BorshDeserialize,
    BorshSerialize,
)]
#[borsh(crate = "borsh")]
#[repr(u8)]
pub enum SideAndOrderTree {
    BidFixed,
    AskFixed,
}

impl SideAndOrderTree {
    pub fn new(side: Side) -> Self {
        match side {
            Side::Bid => Self::BidFixed,
            Side::Ask => Self::AskFixed,
        }
    }

    pub fn side(&self) -> Side {
        match self {
            Self::BidFixed => Side::Bid,
            Self::AskFixed => Side::Ask,
        }
    }
}
