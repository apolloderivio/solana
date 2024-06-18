use super::super::error::MangoError;
use super::*;
use crate::{program_error::ProgramError, pubkey::Pubkey};
use bytemuck::cast;
use sha2::digest::consts::False;
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
        mango_account_pk: &Pubkey,
        now_ts: u64,
        mut limit: u8,
    ) -> std::result::Result<Option<u128>, ProgramError> {
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
        let mut decremented_base_lots = 0i64;
        let mut decremented_quote_lots = 0i64;
        let mut orders_to_change: Vec<(NodeHandle, i64)> = vec![];
        let mut orders_to_delete: Vec<(NodeHandle, u128)> = vec![];
        let mut number_of_dropped_expired_orders = 0;
        let opposing_bookside = self.bookside_mut(other_side);
        for best_opposing in opposing_bookside.iter_all_including_invalid(now_ts) {
            if remaining_base_lots == 0 || remaining_quote_lots == 0 {
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

            let order_would_self_trade = *mango_account_pk == best_opposing.node.owner;
            if order_would_self_trade {
                match order.self_trade_behavior {
                    SelfTradeBehavior::DecrementTake => {
                        // remember all decremented quote lots to only charge fees on not-self-trades
                        decremented_quote_lots += match_quote_lots;
                        // decremented base lots are only tracked for logging
                        decremented_base_lots += match_base_lots;
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
                        return Err(MangoError::WouldSelfTrade.into())
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
                *mango_account_pk,
                order.client_order_id,
                best_opposing_price,
                match_base_lots,
            );
            event_queue.push_back(cast(fill)).unwrap();
            limit -= 1;

            // emit_stack(FilledPerpOrderLog {
            //     mango_group: market.group.key(),
            //     perp_market_index: market.perp_market_index,
            //     seq_num,
            // });
        }
        let total_quote_lots_taken = order.max_quote_lots - remaining_quote_lots;
        let total_base_lots_taken = order.max_base_lots - remaining_base_lots;
        assert!(total_quote_lots_taken >= 0);
        assert!(total_base_lots_taken >= 0);

        // Record the taker trade in the account already, even though it will only be
        // realized when the fill event gets executed
        // if total_quote_lots_taken > 0 || total_base_lots_taken > 0 {
        //     perp_position.add_taker_trade(side, total_base_lots_taken, total_quote_lots_taken);
        //     // reduce fees to apply by decrement take volume
        //     let taker_fees_paid = apply_fees(
        //         market,
        //         mango_account,
        //         total_quote_lots_taken - decremented_quote_lots,
        //     )?;
        //     emit_stack(PerpTakerTradeLog {
        //         mango_group: market.group.key(),
        //         mango_account: *mango_account_pk,
        //         perp_market_index: market.perp_market_index,
        //         taker_side: side as u8,
        //         total_base_lots_taken,
        //         total_base_lots_decremented: decremented_base_lots,
        //         total_quote_lots_taken,
        //         total_quote_lots_decremented: decremented_quote_lots,
        //         taker_fees_paid: taker_fees_paid.to_bits(),
        //         fee_penalty: fee_penalty.to_bits(),
        //     });
        // }

        // Apply changes to matched asks (handles invalidate on delete!)
        for (handle, new_quantity) in orders_to_change {
            opposing_bookside
                .node_mut(handle)
                .unwrap()
                .as_leaf_mut()
                .unwrap()
                .quantity = new_quantity;
        }
        for (component, key) in orders_to_delete {
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
                // MangoErrorCode::OutOfSpace
                if !side.is_price_better(price_lots, worst_price) {
                    return Err(MangoError::SomeError.into());
                }
                // require!(
                //     side.is_price_better(price_lots, worst_price),
                //     MangoError::SomeError
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
                *mango_account_pk,
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

    /// Cancels up to `limit` orders that are listed on the mango account for the given perp market.
    /// Optionally filters by `side_to_cancel_option`.
    /// The orders are removed from the book and from the mango account open order list.
    pub fn cancel_all_orders(
        &mut self,
        mango_account_pk: &Pubkey,
        perp_market: &mut PerpMarket,
        mut limit: u8,
        side_to_cancel_option: Option<Side>,
    ) -> std::result::Result<(), ProgramError> {
        todo!();
    }

    /// Cancels an order on a side, removing it from the book
    pub fn cancel_order_by_id(
        &mut self,
        mango_account_pk: &Pubkey,
        order_id: u128,
        side: Side,
    ) -> Result<LeafNode, ProgramError> {
        let leaf_node = self
            .bookside_mut(side)
            .remove_by_key(order_id)
            .ok_or_else(|| {
                // possibly already filled or expired?
                // error_msg_typed!(MangoError::PerpOrderIdNotFound, "no perp order with id {order_id}, side {side:?}, component {book_component:?} found on the orderbook")
                MangoError::PerpOrderIdNotFound
            })?;
        if leaf_node.owner != *mango_account_pk {
            return Err(MangoError::SomeError.into());
        }
        Ok(leaf_node)
    }
}
