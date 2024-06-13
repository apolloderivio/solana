use super::*;

pub struct BookSideIterItem<'a> {
    pub handle: NodeHandle,
    pub node: &'a LeafNode,
    pub price_lots: i64,
    pub state: OrderState,
}

impl<'a> BookSideIterItem<'a> {
    pub fn is_valid(&self) -> bool {
        self.state == OrderState::Valid
    }
}

/// Iterates the fixed and oracle_pegged OrderTrees simultaneously, allowing users to
/// walk the orderbook without caring about where an order came from.
///
/// This will skip over orders that are not currently matchable, but might be valid
/// in the future.
///
/// This may return invalid orders (tif expired, peg_limit exceeded; see is_valid) which
/// users are supposed to remove from the orderbook if they can.
pub struct BookSideIter<'a> {
    fixed_iter: OrderTreeIter<'a>,
    now_ts: u64,
}

impl<'a> BookSideIter<'a> {
    pub fn new(book_side: &'a BookSide, now_ts: u64) -> Self {
        Self {
            fixed_iter: book_side.nodes.iter(book_side.root()),
            now_ts,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum OrderState {
    Valid,
    Invalid,
    Skipped,
}

/// Returns the state and current price of an oracle pegged order.
///
/// For pegged orders with offsets that let the price escape the 1..i64::MAX range,
/// this function returns Skipped and clamps `price` to that range.
///
/// Orders that exceed their peg_limit will have Invalid state.
fn oracle_pegged_price(oracle_price_lots: i64, node: &LeafNode, side: Side) -> (OrderState, i64) {
    let price_data = node.price_data();
    let price_offset = oracle_pegged_price_offset(price_data);
    let price = oracle_price_lots.saturating_add(price_offset);
    if (1..i64::MAX).contains(&price) {
        if node.peg_limit != -1 && side.is_price_better(price, node.peg_limit) {
            return (OrderState::Invalid, price);
        } else {
            return (OrderState::Valid, price);
        }
    }
    (OrderState::Skipped, price.max(1))
}

/// Replace the price data in a binary tree `key` with the fixed order price data at `price_lots`.
///
/// Used to convert oracle pegged keys into a form that allows comparison with fixed order keys.
fn key_for_fixed_price(key: u128, price_lots: i64) -> u128 {
    // We know this can never fail, because oracle pegged price will always be >= 1
    assert!(price_lots >= 1);
    let price_data = fixed_price_data(price_lots).unwrap();
    let upper = (price_data as u128) << 64;
    let lower = (key as u64) as u128;
    upper | lower
}

/// Helper for the iterator returning a fixed order
fn fixed_to_result(fixed: (NodeHandle, &LeafNode), now_ts: u64) -> BookSideIterItem {
    let (handle, node) = fixed;
    let expired = node.is_expired(now_ts);
    BookSideIterItem {
        handle,
        node,
        price_lots: fixed_price_lots(node.price_data()),
        state: if expired {
            OrderState::Invalid
        } else {
            OrderState::Valid
        },
    }
}

/// Compares the `fixed` and `oracle_pegged` order and returns the one that would match first.
///
/// (or the worse one, if `return_worse` is set)
pub fn rank_orders<'a>(
    side: Side,
    fixed: Option<(NodeHandle, &'a LeafNode)>,
    now_ts: u64,
) -> Option<BookSideIterItem<'a>> {
    // Enrich with data that'll always be needed
    match fixed {
        Some(f) => Some(fixed_to_result(f, now_ts)),
        None => None,
    }
}

impl<'a> Iterator for BookSideIter<'a> {
    type Item = BookSideIterItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let side = self.fixed_iter.side();

        let f_peek = self.fixed_iter.peek();

        let better = rank_orders(side, f_peek, self.now_ts)?;
        self.fixed_iter.next();
        Some(better)
    }
}
