use super::super::error::OrderbookError;
use super::*;
use crate::pubkey::Pubkey;
use bytemuck::cast;
use std::cell::RefMut;

/// Drop at most this many expired orders from a BookSide when trying to match orders.
/// This exists as a guard against excessive compute use.
const DROP_EXPIRED_ORDER_LIMIT: usize = 5;

pub struct Orderbook<'a> {
    pub bids: RefMut<'a, BookSide>,
    pub asks: RefMut<'a, BookSide>,
}

impl<'a> Orderbook<'a> {
    pub fn init(&mut self) {
        self.bids.nodes.order_tree_type = OrderTreeType::Bids.into();
        self.asks.nodes.order_tree_type = OrderTreeType::Asks.into();
    }

    pub fn bookside_mut(&mut self, side: Side) -> &mut BookSide {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    pub fn bookside(&self, side: Side) -> &BookSide {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_order(
        &mut self,
        order: Order,
        market: &mut PerpMarket,
        event_queue: &mut EventQueue,
        account_pk: &Pubkey,
        now_ts: u64,
        mut limit: u8,
    ) -> std::result::Result<Option<u128>, OrderbookError> {
        let side = order.side;
        let other_side = side.invert_side();
        let post_only = order.is_post_only();
        let mut post_target: bool = true;
        let (price_lots, price_data) = order.price()?;
        let order_id = market.gen_order_id(side, price_data);

        // Iterate through book and match against this new order.
        //
        // Any changes to matching orders on the other side of the book are collected in
        // matched_changes/matched_deletes and then applied after this loop.
        let mut remaining_base_lots = order.max_base_lots;
        let mut remaining_quote_lots = order.max_quote_lots;
        let mut orders_to_change: Vec<(NodeHandle, i64)> = vec![];
        let mut orders_to_delete: Vec<(NodeHandle, u128)> = vec![];
        let mut number_of_dropped_expired_orders = 0;
        let opposing_bookside = self.bookside_mut(other_side);
        for best_opposing in opposing_bookside.iter_all_including_invalid(now_ts) {
            if remaining_base_lots == 0 || remaining_quote_lots == 0 {
                println!("remaining_base_lots: {}, break here.", remaining_base_lots);
                break;
            }

            if !best_opposing.is_valid() {
                // Remove the order from the book unless we've done that enough
                if number_of_dropped_expired_orders < DROP_EXPIRED_ORDER_LIMIT {
                    number_of_dropped_expired_orders += 1;
                    let event = OutEvent::from_leaf_node(
                        other_side,
                        now_ts,
                        event_queue.header.seq_num,
                        best_opposing.node,
                    );
                    event_queue.push_back(cast(event)).unwrap();
                    orders_to_delete.push((best_opposing.handle, best_opposing.node.key));
                }
                continue;
            }

            let best_opposing_price = best_opposing.price_lots;

            if !side.is_price_within_limit(best_opposing_price, price_lots) {
                break;
            } else if post_only {
                // msg!("Order could not be placed due to PostOnly");
                post_target = false;
                break; // return silently to not fail other instructions in tx
            } else if limit == 0 {
                // msg!("Order matching limit reached");
                post_target = false;
                break;
            }

            let max_match_by_quote = remaining_quote_lots / best_opposing_price;
            if max_match_by_quote == 0 {
                break;
            }

            let match_base_lots = remaining_base_lots
                .min(best_opposing.node.quantity)
                .min(max_match_by_quote);
            let match_quote_lots = match_base_lots * best_opposing_price;

            let order_would_self_trade = *account_pk == best_opposing.node.owner;
            if order_would_self_trade {
                match order.self_trade_behavior {
                    SelfTradeBehavior::DecrementTake => {
                        // do nothing, just match
                    }
                    SelfTradeBehavior::CancelProvide => {
                        let event = OutEvent::from_leaf_node(
                            other_side,
                            now_ts,
                            event_queue.header.seq_num,
                            best_opposing.node,
                        );
                        event_queue.push_back(cast(event)).unwrap();
                        orders_to_delete.push((best_opposing.handle, best_opposing.node.key));

                        // skip actual matching
                        continue;
                    }
                    SelfTradeBehavior::AbortTransaction => {
                        return Err(OrderbookError::WouldSelfTrade)
                    }
                }
                assert!(order.self_trade_behavior == SelfTradeBehavior::DecrementTake);
            }

            remaining_base_lots -= match_base_lots;
            remaining_quote_lots -= match_quote_lots;
            assert!(remaining_quote_lots >= 0);

            let new_best_opposing_quantity = best_opposing.node.quantity - match_base_lots;
            let maker_out = new_best_opposing_quantity == 0;
            if maker_out {
                orders_to_delete.push((best_opposing.handle, best_opposing.node.key));
            } else {
                orders_to_change.push((best_opposing.handle, new_best_opposing_quantity));
            }

            // order_would_self_trade is only true in the DecrementTake case, in which we don't charge fees
            let seq_num = event_queue.header.seq_num;
            let fill = FillEvent::new(
                side,
                maker_out,
                now_ts,
                seq_num,
                best_opposing.node.owner,
                best_opposing.node.key,
                best_opposing.node.client_order_id,
                best_opposing.node.timestamp,
                *account_pk,
                order.client_order_id,
                best_opposing_price,
                match_base_lots,
            );
            event_queue.push_back(cast(fill)).unwrap();
            limit -= 1;
        }

        let total_quote_lots_taken = order.max_quote_lots - remaining_quote_lots;
        let total_base_lots_taken = order.max_base_lots - remaining_base_lots;
        assert!(total_quote_lots_taken >= 0);
        assert!(total_base_lots_taken >= 0);

        // Apply changes to matched asks (handles invalidate on delete!)
        for (handle, new_quantity) in orders_to_change {
            opposing_bookside
                .node_mut(handle)
                .unwrap()
                .as_leaf_mut()
                .unwrap()
                .quantity = new_quantity;
        }
        for (_component, key) in orders_to_delete {
            let _removed_leaf = opposing_bookside.remove_by_key(key).unwrap();
        }

        //
        // Place remainder on the book if requested
        //

        // If there are still quantity unmatched, place on the book
        let book_base_quantity = remaining_base_lots.min(remaining_quote_lots / price_lots);
        if book_base_quantity <= 0 {
            post_target = false;
        }
        // if post_target.is_some() {
        //     // price limit check computed lazily to save CU on average
        //     let native_price = market.lot_to_native_price(price_lots);
        //     if !market.inside_price_limit(side, native_price, oracle_price) {
        //         msg!("Posting on book disallowed due to price limits, order price {:?}, oracle price {:?}", native_price, oracle_price);
        //         post_target = None;
        //     }
        // }
        if post_target {
            let bookside = self.bookside_mut(side);

            // Drop an expired order if possible
            if let Some(expired_order) = bookside.remove_one_expired(now_ts) {
                let event = OutEvent::from_leaf_node(
                    side,
                    now_ts,
                    event_queue.header.seq_num,
                    &expired_order,
                );
                event_queue.push_back(cast(event)).unwrap();
            }

            if bookside.is_full() {
                // If this bid is higher than lowest bid, boot that bid and insert this one
                let (worst_order, worst_price) = bookside.remove_worst(now_ts).unwrap();
                // OrderbookErrorCode::OutOfSpace
                if !side.is_price_better(price_lots, worst_price) {
                    return Err(OrderbookError::SomeError);
                }
                // require!(
                //     side.is_price_better(price_lots, worst_price),
                //     OrderbookError::SomeError
                // );
                let event = OutEvent::from_leaf_node(
                    side,
                    now_ts,
                    event_queue.header.seq_num,
                    &worst_order,
                );
                event_queue.push_back(cast(event)).unwrap();
            }
            // let owner_slot = mango_account.perp_next_order_slot()?;
            let new_order = LeafNode::new(
                // owner_slot as u8,
                order_id,
                *account_pk,
                book_base_quantity,
                now_ts,
                PostOrderType::Limit, // TODO: Support order types? needed?
                order.time_in_force,
                order.client_order_id,
            );
            let _result = bookside.insert_leaf(&new_order)?;

            // TODO OPT remove if PlacePerpOrder needs more compute
            // msg!(
            //     "{} on book order_id={} quantity={} price={}",
            //     match side {
            //         Side::Bid => "bid",
            //         Side::Ask => "ask",
            //     },
            //     order_id,
            //     book_base_quantity,
            //     price_lots
            // );
            // mango_account.add_perp_order(
            //     market.perp_market_index,
            //     side,
            //     order_tree_target,
            //     &new_order,
            // )?;
        }

        if post_target {
            Ok(Some(order_id))
        } else {
            Ok(None)
        }
    }

    /// Cancels an order on a side, removing it from the book
    pub fn cancel_order_by_id(
        &mut self,
        account_pk: &Pubkey,
        id: u128,
        side: Side,
    ) -> Result<LeafNode, OrderbookError> {
        let leaf_node = self.bookside_mut(side).remove_by_key(id).ok_or_else(|| {
            // possibly already filled or expired?
            // error_msg_typed!(OrderbookError::PerpOrderIdNotFound, "no perp order with id {order_id}, side {side:?}, component {book_component:?} found on the orderbook")
            OrderbookError::PerpOrderIdNotFound
        })?;
        if leaf_node.owner != *account_pk {
            return Err(OrderbookError::SomeError);
        }
        Ok(leaf_node)
    }
}

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
         -> Option<u128> {
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
        let id: Option<u128> = book
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
            .unwrap();
        let order = order_tree_leaf_by_key(&book.bids, id.unwrap()).unwrap();
        assert_eq!(order.client_order_id, 42);
        assert_eq!(order.quantity, bid_quantity);
        assert!(order_tree_contains_key(&book.bids, id.unwrap()));
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
            .unwrap();
        assert_eq!(id2, None);
        // the remainder of the maker order is still on the book
        // (the maker account is unchanged: it was not even passed in)
        let order = order_tree_leaf_by_key(&book.bids, id.unwrap()).unwrap();
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
         -> Option<u128> {
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
