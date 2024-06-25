use super::*;
use crate::program_error::ProgramError;
use borsh::{BorshDeserialize, BorshSerialize};
use static_assertions::const_assert_eq;

#[repr(C)]
#[derive(Copy, Clone, BorshDeserialize, BorshSerialize, bytemuck::Pod, bytemuck::Zeroable)]
#[borsh(crate = "borsh")]
pub struct BookSide {
    pub root: OrderTreeRoot,
    pub nodes: OrderTreeNodes,
}
const_assert_eq!(
    std::mem::size_of::<BookSide>(),
    std::mem::size_of::<OrderTreeNodes>() + std::mem::size_of::<OrderTreeRoot>()
);
const_assert_eq!(std::mem::size_of::<BookSide>(), 123416);
const_assert_eq!(std::mem::size_of::<BookSide>() % 8, 0);

impl BookSide {
    /// Iterate over all entries in the book filtering out invalid orders
    ///
    /// smallest to highest for asks
    /// highest to smallest for bids
    pub fn iter_valid(&self, now_ts: u64) -> impl Iterator<Item = BookSideIterItem> {
        BookSideIter::new(self, now_ts).filter(|it| it.is_valid())
    }

    /// Iterate over all entries, including invalid orders
    pub fn iter_all_including_invalid(&self, now_ts: u64) -> BookSideIter {
        BookSideIter::new(self, now_ts)
    }

    pub fn node(&self, handle: NodeHandle) -> Option<&AnyNode> {
        self.nodes.node(handle)
    }

    pub fn node_mut(&mut self, handle: NodeHandle) -> Option<&mut AnyNode> {
        self.nodes.node_mut(handle)
    }

    pub fn root(&self) -> &OrderTreeRoot {
        &self.root
    }

    pub fn root_mut(&mut self) -> &mut OrderTreeRoot {
        &mut self.root
    }

    pub fn is_full(&self) -> bool {
        self.nodes.is_full()
    }

    pub fn insert_leaf(
        &mut self,
        new_leaf: &LeafNode,
    ) -> Result<(NodeHandle, Option<LeafNode>), ProgramError> {
        self.nodes.insert_leaf(&mut self.root, new_leaf)
    }

    /// Remove the overall worst-price order.
    pub fn remove_worst(&mut self, now_ts: u64) -> Option<(LeafNode, i64)> {
        let worst_fixed = self.nodes.find_worst(&self.root);
        let worse = rank_orders(worst_fixed, now_ts)?;
        let price = worse.price_lots;
        let key = worse.node.key;
        let n = self.remove_by_key(key)?;
        Some((n, price))
    }

    /// Remove the order with the lowest expiry timestamp in the component, if that's < now_ts.
    /// If there is none, try to remove the lowest expiry one from the other component.
    pub fn remove_one_expired(&mut self, now_ts: u64) -> Option<LeafNode> {
        self.nodes.remove_one_expired(&mut self.root, now_ts)
    }

    pub fn remove_by_key(&mut self, search_key: u128) -> Option<LeafNode> {
        self.nodes.remove_by_key(&mut self.root, search_key)
    }

    pub fn side(&self) -> Side {
        self.nodes.order_tree_type().side()
    }

    /// Return the quantity of orders that can be matched by an order at `limit_price_lots`
    pub fn quantity_at_price(&self, limit_price_lots: i64, now_ts: u64) -> i64 {
        let side = self.side();
        let mut sum = 0;
        for item in self.iter_valid(now_ts) {
            if side.is_price_better(limit_price_lots, item.price_lots) {
                break;
            }
            sum += item.node.quantity;
        }
        sum
    }

    /// Return the price of the order closest to the spread
    pub fn best_price(&self, now_ts: u64) -> Option<i64> {
        Some(self.iter_valid(now_ts).next()?.price_lots)
    }

    /// Walk up the book `quantity` units and return the price at that level. If `quantity` units
    /// not on book, return None
    pub fn impact_price(&self, quantity: i64, now_ts: u64) -> Option<i64> {
        let mut sum: i64 = 0;
        for order in self.iter_valid(now_ts) {
            sum += order.node.quantity;
            if sum >= quantity {
                return Some(order.price_lots);
            }
        }
        None
    }

    /// Walk up the book given base units and return the amount in quote lots an order would
    /// be filled at. If not enough liquidity is on book, return None
    pub fn matched_amount(&self, quantity: i64, now_ts: u64) -> Option<i64> {
        if quantity <= 0 {
            return None;
        }
        let mut sum_qty: i64 = 0;
        let mut sum_amt: i64 = 0;
        for order in self.iter_valid(now_ts) {
            sum_qty += order.node.quantity;
            sum_amt += order.node.quantity * order.price_lots;
            let extra_qty = sum_qty - quantity;
            if extra_qty >= 0 {
                sum_amt -= extra_qty * order.price_lots;
                return Some(sum_amt);
            }
        }
        None
    }

    /// Walk up the book given quote units and return the quantity in base lots
    /// an order would need to request to match at least the requested amount.
    /// If not enough liquidity is on book, return None
    pub fn matched_quantity(&self, amount: i64, now_ts: u64) -> Option<i64> {
        if amount <= 0 {
            return None;
        }
        let mut sum_qty: i64 = 0;
        let mut sum_amt: i64 = 0;
        for order in self.iter_valid(now_ts) {
            sum_qty += order.node.quantity;
            sum_amt += order.node.quantity * order.price_lots;
            let extra_amt = sum_amt - amount;
            if extra_amt >= 0 {
                // adding n-1 before dividing through n to force rounding up
                sum_qty -= (extra_amt + order.price_lots - 1) / order.price_lots;
                return Some(sum_qty);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshDeserialize;
    use borsh::BorshSerialize;
    use bytemuck::Zeroable;
    use solana_program::pubkey::Pubkey;
    use std::cell::RefCell;

    fn new_order_tree(order_tree_type: OrderTreeType) -> OrderTreeNodes {
        let mut ot = OrderTreeNodes::zeroed();
        ot.order_tree_type = order_tree_type.into();
        ot
    }

    fn bookside_setup_advanced(fixed: &[(i64, u16)], side: Side) -> BookSide {
        let order_tree_type = match side {
            Side::Bid => OrderTreeType::Bids,
            Side::Ask => OrderTreeType::Asks,
        };

        let order_tree = RefCell::new(new_order_tree(order_tree_type));
        let mut root = OrderTreeRoot::zeroed();
        let new_node = |key: u128, tif: u16| {
            LeafNode::new(
                key,
                Pubkey::default(),
                0,
                1000,
                PostOrderType::Limit,
                tif,
                0,
            )
        };
        let mut add_fixed = |price: i64, tif: u16| {
            let key = new_node_key(side, fixed_price_data(price).unwrap(), 0);
            order_tree
                .borrow_mut()
                .insert_leaf(&mut root, &new_node(key, tif))
                .unwrap();
        };

        for (price, tif) in fixed {
            add_fixed(*price, *tif);
        }

        BookSide {
            root,
            nodes: order_tree.into_inner(),
        }
    }

    fn bookside_setup() -> BookSide {
        bookside_setup_advanced(&[(100, 0), (120, 5)], Side::Bid)
    }

    #[test]
    fn test_borsh_serialization_bookside() {
        smol::block_on(async {
            std::thread::Builder::new()
                .stack_size(32 * 1024 * 1024) // Increase the stack size to 32 MB
                .spawn(|| {
                    let bookside = bookside_setup();
                    let mut serialized_data = Vec::new();
                    bookside
                        .serialize(&mut serialized_data)
                        .expect("BookSide serialization failed");

                    let deserialized: BookSide =
                        BookSide::try_from_slice(&serialized_data).unwrap();
                    assert_eq!(bookside.root.maybe_node, deserialized.root.maybe_node);
                    assert_eq!(bookside.root.leaf_count, deserialized.root.leaf_count);
                    // TODO: more specific assertions
                })
                .unwrap()
                .join()
                .unwrap();
        });
    }

    fn bookside_iteration_random_helper(side: Side) {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        let order_tree_type = match side {
            Side::Bid => OrderTreeType::Bids,
            Side::Ask => OrderTreeType::Asks,
        };

        let mut order_tree = new_order_tree(order_tree_type);
        let mut root = OrderTreeRoot {
            maybe_node: 0,
            leaf_count: 0,
        };
        let new_leaf =
            |key: u128| LeafNode::new(key, Pubkey::default(), 0, 1, PostOrderType::Limit, 0, 0);

        // add 100 leaves to each BookSide, mostly random
        let mut keys = vec![];

        // ensure at least one oracle pegged order visible even at oracle price 1
        let key = new_node_key(side, 20, 0);
        keys.push(key);
        order_tree.insert_leaf(&mut root, &new_leaf(key)).unwrap();

        while root.leaf_count < 100 {
            let price_data: u64 = rng.gen_range(1..50);
            let seq_num: u64 = rng.gen_range(0..1000);
            let key = new_node_key(side, price_data, seq_num);
            if keys.contains(&key) {
                continue;
            }
            keys.push(key);
            order_tree.insert_leaf(&mut root, &new_leaf(key)).unwrap();
        }

        let bookside = BookSide {
            root,
            nodes: order_tree,
        };

        // Verify the order of prices
        let ascending = order_tree_type == OrderTreeType::Asks;
        let mut last_price = if ascending { 0 } else { i64::MAX };

        for order in bookside.iter_all_including_invalid(0) {
            let price = order.price_lots;
            if ascending {
                assert!(price >= last_price, "Prices are not in ascending order");
            } else {
                assert!(price <= last_price, "Prices are not in descending order");
            }
            last_price = price;
        }
    }

    #[test]
    fn bookside_iteration_random() {
        for _i in 0..10 {
            bookside_iteration_random_helper(Side::Bid);
            bookside_iteration_random_helper(Side::Ask);
        }
    }

    #[test]
    fn bookside_order_filtering() {
        let bookside = bookside_setup();

        let order_prices = |now_ts: u64| -> Vec<i64> {
            bookside
                .iter_valid(now_ts)
                .map(|it| it.price_lots)
                .collect()
        };

        // Correct the expected values based on the setup
        assert_eq!(order_prices(0), vec![120, 100]);
        assert_eq!(order_prices(1004), vec![120, 100]);
        assert_eq!(order_prices(1005), vec![100]);
        assert_eq!(order_prices(1006), vec![100]);
        assert_eq!(order_prices(1007), vec![100]);
    }

    #[test]
    fn bookside_remove_worst() {
        use std::cell::RefCell;

        let bookside = RefCell::new(bookside_setup());

        let order_prices = |now_ts: u64| -> Vec<i64> {
            bookside
                .borrow()
                .iter_valid(now_ts)
                .map(|it| it.price_lots)
                .collect()
        };

        // Initial setup expectations
        assert_eq!(order_prices(0), vec![120, 100]);

        // Remove the worst order and verify
        let (_, p) = bookside.borrow_mut().remove_worst(0).unwrap();
        assert_eq!(p, 100);
        assert_eq!(order_prices(0), vec![120]);

        // Further removals to ensure the functionality
        let (_, p) = bookside.borrow_mut().remove_worst(0).unwrap();
        assert_eq!(p, 120);
        assert_eq!(order_prices(0), Vec::<i64>::new());
    }

    fn bookside_setup_complex(fixed: &[(i64, u16, u64)], side: Side) -> BookSide {
        let order_tree_type = match side {
            Side::Bid => OrderTreeType::Bids,
            Side::Ask => OrderTreeType::Asks,
        };

        let order_tree = RefCell::new(new_order_tree(order_tree_type));
        let mut root = OrderTreeRoot::zeroed();

        let new_node = |price: i64, tif: u16, ts: u64| {
            LeafNode::new(
                new_node_key(side, fixed_price_data(price).unwrap(), 0),
                Pubkey::default(),
                1000, // quantity
                ts,   // timestamp
                PostOrderType::Limit,
                tif,
                0, // client_order_id
            )
        };
        let mut add_fixed = |price: i64, tif: u16, ts: u64| {
            order_tree
                .borrow_mut()
                .insert_leaf(&mut root, &new_node(price, tif, ts))
                .unwrap();
        };

        for (price, tif, ts) in fixed {
            add_fixed(*price, *tif, *ts);
        }

        BookSide {
            root,
            nodes: order_tree.into_inner(),
        }
    }

    #[test]
    fn test_iter_valid() {
        let bookside = bookside_setup_complex(
            &[
                (100, 0, 10), // not expire
                (120, 5, 20), // expire at 25
                (130, 3, 15), // expire at 18
                (110, 7, 25), // expire at 32
                (140, 2, 5),  // expire at 7
            ],
            Side::Bid,
        );

        // Now at timestamp 0, all orders should be valid
        let now_ts = 0;
        let prices: Vec<i64> = bookside
            .iter_valid(now_ts)
            .map(|it| it.price_lots)
            .collect();
        assert_eq!(prices, vec![140, 130, 120, 110, 100]);
        assert_eq!(bookside.impact_price(3000, now_ts).unwrap(), 120);
        assert_eq!(
            bookside
                .matched_quantity((140 + 130 + 120) * 1000, now_ts)
                .unwrap(),
            3000
        );

        // Now at timestamp 7, orders expiring at or before 20 should be excluded
        let now_ts = 7;
        let prices: Vec<i64> = bookside
            .iter_valid(now_ts)
            .map(|it| it.price_lots)
            .collect();
        assert_eq!(prices, vec![130, 120, 110, 100]);
        assert_eq!(bookside.impact_price(3000, now_ts).unwrap(), 110);
        assert_eq!(
            bookside
                .matched_quantity((140 + 130 + 120) * 1000, now_ts)
                .unwrap(),
            3300
        );

        // Now at timestamp 10, orders expiring at or before 10 should be excluded
        let now_ts = 10;
        let prices: Vec<i64> = bookside
            .iter_valid(now_ts)
            .map(|it| it.price_lots)
            .collect();
        assert_eq!(prices, vec![130, 120, 110, 100]);
        assert_eq!(bookside.impact_price(3000, now_ts).unwrap(), 110);
        assert_eq!(
            bookside
                .matched_quantity((140 + 130 + 120) * 1000, now_ts)
                .unwrap(),
            3300
        );

        // Now at timestamp 25, orders expiring at or before 15 should be excluded
        let now_ts = 25;
        let prices: Vec<i64> = bookside
            .iter_valid(now_ts)
            .map(|it| it.price_lots)
            .collect();
        assert_eq!(prices, vec![110, 100]);
        assert_eq!(bookside.impact_price(3000, now_ts), None);
        assert_eq!(
            bookside.matched_quantity((140 + 130 + 120) * 1000, now_ts),
            None
        );

        // Now at timestamp 32, only order not expired exists
        let now_ts = 32;
        let prices: Vec<i64> = bookside
            .iter_valid(now_ts)
            .map(|it| it.price_lots)
            .collect();
        assert_eq!(prices, vec![100]);
        assert_eq!(bookside.impact_price(3000, now_ts), None);
        assert_eq!(
            bookside.matched_quantity((140 + 130 + 120) * 1000, now_ts),
            None
        );
    }
}
