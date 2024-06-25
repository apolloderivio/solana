use super::{LeafNode, Side};
use crate::{orderbook::error::OrderbookError, pubkey::Pubkey};
use borsh::{BorshDeserialize, BorshSerialize};
use bytemuck::{cast_ref, Pod, Zeroable};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use static_assertions::const_assert_eq;
use std::mem::size_of;

pub const MAX_NUM_EVENTS: usize = 488;

pub trait QueueHeader: bytemuck::Pod {
    type Item: bytemuck::Pod + Copy;

    fn head(&self) -> usize;
    fn set_head(&mut self, value: u32);
    fn count(&self) -> usize;
    fn set_count(&mut self, value: u32);

    fn incr_event_id(&mut self);
    fn decr_event_id(&mut self, n: u64);
}

#[repr(C)]
#[derive(Copy, Clone, Debug, BorshDeserialize, BorshSerialize)]
#[borsh(crate = "borsh")]
pub struct EventQueue {
    pub header: EventQueueHeader,
    pub buf: [AnyEvent; MAX_NUM_EVENTS],
    pub reserved: [u8; 64],
}
unsafe impl bytemuck::Pod for EventQueue {}
unsafe impl bytemuck::Zeroable for EventQueue {}

const_assert_eq!(std::mem::size_of::<EventQueue>(), 16 + 488 * 192 + 64);
const_assert_eq!(std::mem::size_of::<EventQueue>(), 93776);
const_assert_eq!(std::mem::size_of::<EventQueue>() % 8, 0);

impl EventQueue {
    pub fn len(&self) -> usize {
        self.header.count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn full(&self) -> bool {
        self.header.count() == self.buf.len()
    }

    pub fn push_back(&mut self, value: AnyEvent) -> std::result::Result<(), AnyEvent> {
        if self.full() {
            return Err(value);
        }
        let slot = (self.header.head() + self.header.count()) % self.buf.len();
        self.buf[slot] = value;

        let count = self.header.count();
        self.header.set_count((count + 1) as u32); // guaranteed because of full() check

        self.header.incr_event_id();
        Ok(())
    }

    pub fn peek_front(&self) -> Option<&AnyEvent> {
        if self.is_empty() {
            return None;
        }
        Some(&self.buf[self.header.head()])
    }

    pub fn peek_front_mut(&mut self) -> Option<&mut AnyEvent> {
        if self.is_empty() {
            return None;
        }
        Some(&mut self.buf[self.header.head()])
    }

    pub fn pop_front(&mut self) -> Result<AnyEvent, OrderbookError> {
        if self.is_empty() {
            return Err(OrderbookError::SomeError);
        }

        let value = self.buf[self.header.head()];

        let count = self.header.count();
        self.header.set_count((count - 1) as u32);

        let head = self.header.head();
        self.header.set_head(((head + 1) % self.buf.len()) as u32);

        Ok(value)
    }

    pub fn revert_pushes(&mut self, desired_len: usize) -> Result<(), OrderbookError> {
        if desired_len > self.header.count() {
            return Err(OrderbookError::SomeError);
        }
        let len_diff = self.header.count() - desired_len;
        self.header.set_count(desired_len as u32);
        self.header.decr_event_id(len_diff as u64);
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = &AnyEvent> {
        EventQueueIterator {
            queue: self,
            index: 0,
        }
    }
}

struct EventQueueIterator<'a> {
    queue: &'a EventQueue,
    index: usize,
}

impl<'a> Iterator for EventQueueIterator<'a> {
    type Item = &'a AnyEvent;
    fn next(&mut self) -> Option<Self::Item> {
        if self.index == self.queue.len() {
            None
        } else {
            let item =
                &self.queue.buf[(self.queue.header.head() + self.index) % self.queue.buf.len()];
            self.index += 1;
            Some(item)
        }
    }
}

#[derive(
    Debug, Copy, Clone, bytemuck::Zeroable, bytemuck::Pod, BorshDeserialize, BorshSerialize,
)]
#[borsh(crate = "borsh")]
#[repr(C)]
pub struct EventQueueHeader {
    head: u32,
    count: u32,
    pub seq_num: u64,
}
const_assert_eq!(std::mem::size_of::<EventQueueHeader>(), 16);
const_assert_eq!(std::mem::size_of::<EventQueueHeader>() % 8, 0);

impl QueueHeader for EventQueueHeader {
    type Item = AnyEvent;

    fn head(&self) -> usize {
        self.head as usize
    }
    fn set_head(&mut self, value: u32) {
        self.head = value;
    }
    fn count(&self) -> usize {
        self.count as usize
    }
    fn set_count(&mut self, value: u32) {
        self.count = value;
    }
    fn incr_event_id(&mut self) {
        self.seq_num += 1;
    }
    fn decr_event_id(&mut self, n: u64) {
        self.seq_num -= n;
    }
}

#[allow(dead_code)]
const EVENT_SIZE: usize = 192;

#[derive(Copy, Clone, Debug, BorshDeserialize, BorshSerialize)]
#[borsh(crate = "borsh")]
#[repr(C)]
pub struct AnyEvent {
    pub event_type: u8,
    pub padding: [u8; 191],
}
unsafe impl bytemuck::Pod for AnyEvent {}
unsafe impl bytemuck::Zeroable for AnyEvent {}

const_assert_eq!(size_of::<AnyEvent>(), EVENT_SIZE);

#[derive(Copy, Clone, IntoPrimitive, TryFromPrimitive, Eq, PartialEq)]
#[repr(u8)]
pub enum EventType {
    Fill,
    Out,
    Liquidate,
}

#[derive(Copy, Clone, Debug, Pod, Zeroable, BorshDeserialize, BorshSerialize)]
#[borsh(crate = "borsh")]
#[repr(C)]
pub struct FillEvent {
    pub event_type: u8,
    pub taker_side: u8, // Side, from the taker's POV
    pub maker_out: u8,  // 1 if maker order quantity == 0
    pub padding: [u8; 5],
    pub timestamp: u64,
    pub seq_num: u64,

    pub maker: Pubkey,
    pub padding2: [u8; 32],

    // Timestamp of when the maker order was placed; copied over from the LeafNode
    pub maker_timestamp: u64,

    pub taker: Pubkey,
    pub padding3: [u8; 16],
    pub taker_client_order_id: u64,
    pub maker_order_id: u128,

    pub price: i64,
    pub quantity: i64, // number of quote lots
    pub maker_client_order_id: u64,
}
const_assert_eq!(size_of::<FillEvent>() % 8, 0);
const_assert_eq!(size_of::<FillEvent>(), EVENT_SIZE);

impl FillEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        taker_side: Side,
        maker_out: bool,
        timestamp: u64,
        seq_num: u64,
        maker: Pubkey,
        maker_order_id: u128,
        maker_client_order_id: u64,
        maker_timestamp: u64,
        taker: Pubkey,
        taker_client_order_id: u64,
        price: i64,
        quantity: i64,
    ) -> FillEvent {
        Self {
            event_type: EventType::Fill as u8,
            taker_side: taker_side.into(),
            maker_out: maker_out.into(),
            timestamp,
            seq_num,
            maker,
            maker_order_id,
            maker_client_order_id,
            maker_timestamp,
            taker,
            taker_client_order_id,
            price,
            quantity,
            padding: Default::default(),
            padding2: Default::default(),
            padding3: Default::default(),
        }
    }

    pub fn base_quote_change(&self, side: Side) -> (i64, i64) {
        match side {
            Side::Bid => (self.quantity, -self.price * self.quantity),
            Side::Ask => (-self.quantity, self.price * self.quantity),
        }
    }

    pub fn taker_side(&self) -> Side {
        self.taker_side.try_into().unwrap()
    }
    pub fn maker_out(&self) -> bool {
        self.maker_out == 1
    }
}

impl TryFrom<AnyEvent> for FillEvent {
    type Error = OrderbookError;

    fn try_from(e: AnyEvent) -> Result<Self, OrderbookError> {
        if e.event_type != EventType::Fill as u8 {
            // Err(error_msg!(
            //     "could not convert event with type={} to FillEvent",
            //     e.event_type
            // ))
            Err(OrderbookError::SomeError)
        } else {
            Ok(*cast_ref(&e))
        }
    }
}

impl<'a> TryFrom<&'a AnyEvent> for &'a FillEvent {
    type Error = OrderbookError;

    fn try_from(e: &'a AnyEvent) -> Result<Self, OrderbookError> {
        if e.event_type != EventType::Fill as u8 {
            // Err(error_msg!(
            //     "could not convert event with type={} to FillEvent",
            //     e.event_type
            // ))
            Err(OrderbookError::SomeError)
        } else {
            Ok(cast_ref(e))
        }
    }
}

#[derive(Copy, Clone, Debug, BorshDeserialize, BorshSerialize)]
#[borsh(crate = "borsh")]
#[repr(C)]
pub struct OutEvent {
    pub event_type: u8,
    pub side: u8, // Side
    padding0: [u8; 6],
    pub timestamp: u64,
    pub seq_num: u64,
    pub owner: Pubkey,
    pub quantity: i64,
    pub order_id: u128,
    padding1: [u8; 112],
}
const_assert_eq!(size_of::<OutEvent>() % 8, 0);
const_assert_eq!(size_of::<OutEvent>(), EVENT_SIZE);
unsafe impl bytemuck::Pod for OutEvent {}
unsafe impl bytemuck::Zeroable for OutEvent {}

impl OutEvent {
    pub fn new(
        side: Side,
        timestamp: u64,
        seq_num: u64,
        owner: Pubkey,
        quantity: i64,
        order_id: u128,
    ) -> Self {
        Self {
            event_type: EventType::Out.into(),
            side: side.into(),
            padding0: [0; 6],
            timestamp,
            seq_num,
            owner,
            quantity,
            order_id,
            padding1: [0; 112],
        }
    }

    pub fn from_leaf_node(side: Side, timestamp: u64, seq_num: u64, node: &LeafNode) -> Self {
        Self::new(
            side,
            timestamp,
            seq_num,
            node.owner,
            node.quantity,
            node.key,
        )
    }

    pub fn side(&self) -> Side {
        self.side.try_into().unwrap()
    }
}

impl TryFrom<AnyEvent> for OutEvent {
    type Error = OrderbookError;

    fn try_from(e: AnyEvent) -> Result<Self, OrderbookError> {
        if e.event_type != EventType::Out as u8 {
            // Err(error_msg!(
            //     "could not convert event with type={} to OutEvent",
            //     e.event_type
            // ))
            Err(OrderbookError::SomeError)
        } else {
            Ok(*cast_ref(&e))
        }
    }
}

impl<'a> TryFrom<&'a AnyEvent> for &'a OutEvent {
    type Error = OrderbookError;

    fn try_from(e: &'a AnyEvent) -> Result<Self, OrderbookError> {
        if e.event_type != EventType::Out as u8 {
            // Err(error_msg!(
            //     "could not convert event with type={} to OutEvent",
            //     e.event_type
            // ))
            Err(OrderbookError::SomeError)
        } else {
            Ok(cast_ref(e))
        }
    }
}
