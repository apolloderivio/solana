pub mod book;
pub mod bookside;
pub mod bookside_iterator;
pub mod nodes;
pub mod order;
pub mod order_type;
pub mod ordertree;
pub mod ordertree_iterator;
pub mod perp_market;
pub mod queue;

pub use book::*;
pub use bookside::*;
pub use bookside_iterator::*;
pub use nodes::*;
pub use order::*;
pub use order_type::*;
pub use ordertree::*;
pub use ordertree_iterator::*;
pub use perp_market::*;
pub use queue::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orderbook::{
        error::OrderbookError,
        state::{BookSide, Orderbook},
    };
    use bytemuck::Zeroable;
    use solana_program::pubkey::Pubkey;
    use std::cell::RefCell;

    fn order_tree_leaf_by_key(bookside: &BookSide, key: u128) -> Option<&LeafNode> {
        for (_, leaf) in bookside.nodes.iter(bookside.root()) {
            if leaf.key == key {
                return Some(leaf);
            }
        }
        None
    }

    fn order_tree_contains_key(bookside: &BookSide, key: u128) -> bool {
        order_tree_leaf_by_key(bookside, key).is_some()
    }

    fn order_tree_contains_price(bookside: &BookSide, price_data: u64) -> bool {
        for (_, leaf) in bookside.nodes.iter(bookside.root()) {
            if leaf.price_data() == price_data {
                return true;
            }
        }
        false
    }

    struct OrderbookAccounts {
        bids: Box<RefCell<BookSide>>,
        asks: Box<RefCell<BookSide>>,
    }

    impl OrderbookAccounts {
        fn new() -> Self {
            let s = Self {
                bids: Box::new(RefCell::new(BookSide::zeroed())),
                asks: Box::new(RefCell::new(BookSide::zeroed())),
            };
            s.bids.borrow_mut().nodes.order_tree_type = OrderTreeType::Bids.into();
            s.asks.borrow_mut().nodes.order_tree_type = OrderTreeType::Asks.into();
            s
        }

        fn orderbook(&self) -> Orderbook {
            Orderbook {
                bids: self.bids.borrow_mut(),
                asks: self.asks.borrow_mut(),
            }
        }
    }

    fn test_setup() -> (PerpMarket, EventQueue, OrderbookAccounts) {
        let book = OrderbookAccounts::new();
        let event_queue = EventQueue::zeroed();
        let mut perp_market = PerpMarket::zeroed();

        perp_market.quote_lot_size = 1;
        perp_market.base_lot_size = 1;

        (perp_market, event_queue, book)
    }

    // Check what happens when one side of the book fills up
    #[test]
    fn book_bids_full() {
        let (mut perp_market, mut event_queue, book_accs) = test_setup();
        let mut book = book_accs.orderbook();

        let mut new_order = |book: &mut Orderbook,
                             event_queue: &mut EventQueue,
                             side,
                             price_lots,
                             now_ts|
         -> u128 {
            let max_base_lots = 1;
            let time_in_force = 100;

            book.new_order(
                Order {
                    side,
                    max_base_lots,
                    max_quote_lots: i64::MAX,
                    client_order_id: 0,
                    time_in_force,
                    reduce_only: false,
                    self_trade_behavior: SelfTradeBehavior::DecrementTake,
                    params: OrderParams::Fixed {
                        price_lots,
                        order_type: PostOrderType::Limit,
                    },
                },
                &mut perp_market,
                event_queue,
                &Pubkey::default(),
                now_ts,
                u8::MAX,
            )
            .unwrap()
            .unwrap()
        };

        // insert bids until book side is full
        for i in 1..10 {
            new_order(
                &mut book,
                &mut event_queue,
                Side::Bid,
                1000 + i as i64,
                1000000 + i as u64,
            );
        }
        for i in 10..1000 {
            new_order(
                &mut book,
                &mut event_queue,
                Side::Bid,
                1000 + i as i64,
                1000011 as u64,
            );
            if book.bids.is_full() {
                break;
            }
        }
        assert!(book.bids.is_full());
        assert_eq!(
            book.bids
                .nodes
                .min_leaf(&book.bids.root)
                .unwrap()
                .1
                .price_data(),
            1001
        );
        assert_eq!(
            fixed_price_lots(
                book.bids
                    .nodes
                    .max_leaf(&book.bids.root)
                    .unwrap()
                    .1
                    .price_data()
            ),
            (1000 + book.bids.root.leaf_count) as i64
        );

        // add another bid at a higher price before expiry, replacing the lowest-price one (1001)
        new_order(&mut book, &mut event_queue, Side::Bid, 1005, 1000000 - 1);
        assert_eq!(
            book.bids
                .nodes
                .min_leaf(&book.bids.root)
                .unwrap()
                .1
                .price_data(),
            1002
        );
        assert_eq!(event_queue.len(), 1);

        // adding another bid after expiry removes the soonest-expiring order (1005)
        new_order(&mut book, &mut event_queue, Side::Bid, 999, 2000000);
        assert_eq!(
            book.bids
                .nodes
                .min_leaf(&book.bids.root)
                .unwrap()
                .1
                .price_data(),
            999
        );
        assert!(!order_tree_contains_key(&book.bids, 1005));
        assert_eq!(event_queue.len(), 2);

        // adding an ask will wipe up to three expired bids at the top of the book
        let bids_max = book
            .bids
            .nodes
            .max_leaf(&book.bids.root)
            .unwrap()
            .1
            .price_data();
        let bids_count = book.bids.root.leaf_count;
        new_order(&mut book, &mut event_queue, Side::Ask, 6000, 1500000);
        assert_eq!(book.bids.root.leaf_count, bids_count - 5);
        assert_eq!(book.asks.root.leaf_count, 1);
        assert_eq!(event_queue.len(), 2 + 5);
        assert!(!order_tree_contains_price(&book.bids, bids_max));
        assert!(!order_tree_contains_price(&book.bids, bids_max - 1));
        assert!(!order_tree_contains_price(&book.bids, bids_max - 2));
        assert!(!order_tree_contains_price(&book.bids, bids_max - 3));
        assert!(!order_tree_contains_price(&book.bids, bids_max - 4));
        assert!(order_tree_contains_price(&book.bids, bids_max - 5));
    }

    #[test]
    fn book_new_order() {
        let (mut market, mut event_queue, book_accs) = test_setup();
        let mut book = book_accs.orderbook();

        // Add lots and fees to make sure to exercise unit conversion
        market.base_lot_size = 10;
        market.quote_lot_size = 100;

        let maker_pk = Pubkey::new_unique();
        let taker_pk = Pubkey::new_unique();
        let now_ts = 1000000;

        // Place a maker-bid
        let price_lots = 1000 * market.base_lot_size / market.quote_lot_size;
        let bid_quantity = 10;
        let id = book
            .new_order(
                Order {
                    side: Side::Bid,
                    max_base_lots: bid_quantity,
                    max_quote_lots: i64::MAX,
                    client_order_id: 42,
                    time_in_force: 0,
                    reduce_only: false,
                    self_trade_behavior: SelfTradeBehavior::DecrementTake,
                    params: OrderParams::Fixed {
                        price_lots,
                        order_type: PostOrderType::Limit,
                    },
                },
                &mut market,
                &mut event_queue,
                &maker_pk,
                now_ts,
                u8::MAX,
            )
            .unwrap()
            .unwrap();
        let order = order_tree_leaf_by_key(&book.bids, id).unwrap();
        assert_eq!(order.client_order_id, 42);
        assert_eq!(order.quantity, bid_quantity);
        assert!(order_tree_contains_key(&book.bids, id));
        assert!(order_tree_contains_price(&book.bids, price_lots as u64));
        assert_eq!(event_queue.len(), 0);

        // Take the order partially
        let match_quantity = 5;
        let id2 = book
            .new_order(
                Order {
                    side: Side::Ask,
                    max_base_lots: match_quantity,
                    max_quote_lots: i64::MAX,
                    client_order_id: 43,
                    time_in_force: 0,
                    reduce_only: false,
                    self_trade_behavior: SelfTradeBehavior::DecrementTake,
                    params: OrderParams::Fixed {
                        price_lots,
                        order_type: PostOrderType::Limit,
                    },
                },
                &mut market,
                &mut event_queue,
                &taker_pk,
                now_ts,
                u8::MAX,
            )
            .unwrap()
            .unwrap();
        // the remainder of the maker order is still on the book
        // (the maker account is unchanged: it was not even passed in)
        let order = order_tree_leaf_by_key(&book.bids, id2).unwrap();
        assert_eq!(fixed_price_lots(order.price_data()), price_lots);
        assert_eq!(order.quantity, bid_quantity - match_quantity);

        // the fill gets added to the event queue
        assert_eq!(event_queue.len(), 1);
        let event = event_queue.peek_front().unwrap();
        assert_eq!(event.event_type, EventType::Fill as u8);
        let fill: &FillEvent = bytemuck::cast_ref(event);
        assert_eq!(fill.quantity, match_quantity);
        assert_eq!(fill.price, price_lots);
        assert_eq!(fill.taker_client_order_id, 43);
        assert_eq!(fill.maker, maker_pk);
        assert_eq!(fill.taker, taker_pk);

        // TODO: simulate event queue processing
    }

    // Check that there are no zero-quantity fills when max_quote_lots is not
    // enough for a single lot
    #[test]
    fn book_max_quote_lots() {
        let (mut perp_market, mut event_queue, book_accs) = test_setup();
        let mut book = book_accs.orderbook();

        let mut new_order = |book: &mut Orderbook,
                             event_queue: &mut EventQueue,
                             side,
                             price_lots,
                             max_base_lots: i64,
                             max_quote_lots: i64|
         -> u128 {
            book.new_order(
                Order {
                    side,
                    max_base_lots,
                    max_quote_lots,
                    client_order_id: 0,
                    time_in_force: 0,
                    reduce_only: false,
                    self_trade_behavior: SelfTradeBehavior::DecrementTake,
                    params: OrderParams::Fixed {
                        price_lots,
                        order_type: PostOrderType::Limit,
                    },
                },
                &mut perp_market,
                event_queue,
                &Pubkey::default(),
                0, // now_ts
                u8::MAX,
            )
            .unwrap()
            .unwrap()
        };

        // Setup
        new_order(&mut book, &mut event_queue, Side::Ask, 5000, 5, i64::MAX);
        new_order(&mut book, &mut event_queue, Side::Ask, 5001, 5, i64::MAX);
        new_order(&mut book, &mut event_queue, Side::Ask, 5002, 5, i64::MAX);

        // Try taking: the quote limit allows only one base lot to be taken.
        new_order(&mut book, &mut event_queue, Side::Bid, 5005, 30, 6000);
        // Only one fill event is generated, the matching aborts even though neither the base nor quote limit
        // is exhausted.
        assert_eq!(event_queue.len(), 1);

        // Try taking: the quote limit allows no fills
        new_order(&mut book, &mut event_queue, Side::Bid, 5005, 30, 1);
        assert_eq!(event_queue.len(), 1);
    }

    #[test]
    fn test_self_trade_decrement_take() -> Result<(), OrderbookError> {
        // setup market
        let (mut market, mut event_queue, book_accs) = test_setup();
        let mut book = book_accs.orderbook();
        let now_ts = 1000000;

        let maker_pk = Pubkey::new_unique();
        let taker_pk = Pubkey::new_unique();

        // taker limit order
        book.new_order(
            Order {
                side: Side::Ask,
                max_base_lots: 2,
                max_quote_lots: i64::MAX,
                client_order_id: 1,
                time_in_force: 0,
                reduce_only: false,
                self_trade_behavior: SelfTradeBehavior::default(),
                params: OrderParams::Fixed {
                    price_lots: 1000,
                    order_type: PostOrderType::Limit,
                },
            },
            &mut market,
            &mut event_queue,
            &taker_pk,
            now_ts,
            u8::MAX,
        )
        .unwrap();

        // maker limit order
        book.new_order(
            Order {
                side: Side::Ask,
                max_base_lots: 2,
                max_quote_lots: i64::MAX,
                client_order_id: 2,
                time_in_force: 0,
                reduce_only: false,
                self_trade_behavior: SelfTradeBehavior::default(),
                params: OrderParams::Fixed {
                    price_lots: 1000,
                    order_type: PostOrderType::Limit,
                },
            },
            &mut market,
            &mut event_queue,
            &maker_pk,
            now_ts,
            u8::MAX,
        )
        .unwrap();

        // taker full self-trade IOC
        book.new_order(
            Order {
                side: Side::Bid,
                max_base_lots: 1,
                max_quote_lots: i64::MAX,
                client_order_id: 3,
                time_in_force: 0,
                reduce_only: false,
                self_trade_behavior: SelfTradeBehavior::DecrementTake,
                params: OrderParams::ImmediateOrCancel { price_lots: 1000 },
            },
            &mut market,
            &mut event_queue,
            &taker_pk,
            now_ts,
            u8::MAX,
        )
        .unwrap();

        let fill_event: FillEvent = event_queue.pop_front()?.try_into()?;
        assert_eq!(fill_event.quantity, 1);
        assert_eq!(fill_event.maker, taker_pk);
        assert_eq!(fill_event.taker, taker_pk);

        //  taker partial self trade limit
        book.new_order(
            Order {
                side: Side::Bid,
                max_base_lots: 2,
                max_quote_lots: i64::MAX,
                client_order_id: 4,
                time_in_force: 0,
                reduce_only: false,
                self_trade_behavior: SelfTradeBehavior::DecrementTake,
                params: OrderParams::Fixed {
                    price_lots: 1000,
                    order_type: PostOrderType::Limit,
                },
            },
            &mut market,
            &mut event_queue,
            &taker_pk,
            now_ts,
            u8::MAX,
        )
        .unwrap();

        let fill_event: FillEvent = event_queue.pop_front()?.try_into()?;
        assert_eq!(fill_event.quantity, 1);
        assert_eq!(fill_event.maker, taker_pk);
        assert_eq!(fill_event.taker, taker_pk);

        let fill_event: FillEvent = event_queue.pop_front()?.try_into()?;
        assert_eq!(fill_event.quantity, 1);
        assert_eq!(fill_event.maker, maker_pk);
        assert_eq!(fill_event.taker, taker_pk);

        Ok(())
    }

    #[test]
    fn test_self_trade_cancel_provide() -> Result<(), OrderbookError> {
        // setup market
        let (mut market, mut event_queue, book_accs) = test_setup();
        let mut book = book_accs.orderbook();
        let now_ts = 1000000;
        market.fee_penalty = 5.0;

        let maker_pk = Pubkey::new_unique();
        let taker_pk = Pubkey::new_unique();
        // taker limit order
        book.new_order(
            Order {
                side: Side::Ask,
                max_base_lots: 1,
                max_quote_lots: i64::MAX,
                client_order_id: 1,
                time_in_force: 0,
                reduce_only: false,
                self_trade_behavior: SelfTradeBehavior::default(),
                params: OrderParams::Fixed {
                    price_lots: 1000,
                    order_type: PostOrderType::Limit,
                },
            },
            &mut market,
            &mut event_queue,
            &taker_pk,
            now_ts,
            u8::MAX,
        )
        .unwrap();

        // maker limit order
        book.new_order(
            Order {
                side: Side::Ask,
                max_base_lots: 2,
                max_quote_lots: i64::MAX,
                client_order_id: 2,
                time_in_force: 0,
                reduce_only: false,
                self_trade_behavior: SelfTradeBehavior::default(),
                params: OrderParams::Fixed {
                    price_lots: 1000,
                    order_type: PostOrderType::Limit,
                },
            },
            &mut market,
            &mut event_queue,
            &maker_pk,
            now_ts,
            u8::MAX,
        )
        .unwrap();

        // taker partial self-trade
        book.new_order(
            Order {
                side: Side::Bid,
                max_base_lots: 1,
                max_quote_lots: i64::MAX,
                client_order_id: 3,
                time_in_force: 0,
                reduce_only: false,
                self_trade_behavior: SelfTradeBehavior::CancelProvide,
                params: OrderParams::Fixed {
                    price_lots: 1000,
                    order_type: PostOrderType::Limit,
                },
            },
            &mut market,
            &mut event_queue,
            &taker_pk,
            now_ts,
            u8::MAX,
        )
        .unwrap();

        let out_event: OutEvent = event_queue.pop_front()?.try_into()?;
        assert_eq!(out_event.owner, taker_pk);

        let fill_event: FillEvent = event_queue.pop_front()?.try_into()?;
        assert_eq!(fill_event.maker, maker_pk);
        assert_eq!(fill_event.taker, taker_pk);
        assert_eq!(fill_event.quantity, 1);

        Ok(())
    }

    #[test]
    fn test_self_trade_abort_transaction() -> Result<(), OrderbookError> {
        // setup market
        let (mut market, mut event_queue, book_accs) = test_setup();
        let mut book = book_accs.orderbook();
        let now_ts = 1000000;
        market.fee_penalty = 5.0;

        let taker_pk = Pubkey::new_unique();

        // taker limit order
        book.new_order(
            Order {
                side: Side::Ask,
                max_base_lots: 1,
                max_quote_lots: i64::MAX,
                client_order_id: 1,
                time_in_force: 0,
                reduce_only: false,
                self_trade_behavior: SelfTradeBehavior::default(),
                params: OrderParams::Fixed {
                    price_lots: 1000,
                    order_type: PostOrderType::Limit,
                },
            },
            &mut market,
            &mut event_queue,
            &taker_pk,
            now_ts,
            u8::MAX,
        )
        .unwrap();

        // taker failing self-trade
        book.new_order(
            Order {
                side: Side::Bid,
                max_base_lots: 1,
                max_quote_lots: i64::MAX,
                client_order_id: 3,
                time_in_force: 0,
                reduce_only: false,
                self_trade_behavior: SelfTradeBehavior::AbortTransaction,
                params: OrderParams::ImmediateOrCancel { price_lots: 1000 },
            },
            &mut market,
            &mut event_queue,
            &taker_pk,
            now_ts,
            u8::MAX,
        )
        .expect_err("should fail");

        Ok(())
    }
}
