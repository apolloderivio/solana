use super::{PostOrderType, Side};
use crate::{program_error::ProgramError, pubkey::Pubkey};
use borsh::{BorshDeserialize, BorshSerialize};
use bytemuck::{cast_mut, cast_ref};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use static_assertions::const_assert_eq;
use std::mem::{align_of, size_of};

pub type NodeHandle = u32;
const NODE_SIZE: usize = 120;

#[derive(Eq, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum NodeTag {
    Uninitialized = 0,
    InnerNode = 1,
    LeafNode = 2,
    FreeNode = 3,
    LastFreeNode = 4,
}

/// Creates a binary tree node key.
///
/// It's used for sorting nodes (ascending for asks, descending for bids)
/// and encodes price data in the top 64 bits followed by an ordering number
/// in the lower bits.
///
/// The `seq_num` that's passed should monotonically increase. It's used to choose
/// the ordering number such that orders placed later for the same price data
/// are ordered after earlier orders.
pub fn new_node_key(side: Side, price_data: u64, seq_num: u64) -> u128 {
    let seq_num = if side == Side::Bid { !seq_num } else { seq_num };

    let upper = (price_data as u128) << 64;
    upper | (seq_num as u128)
}

/// Creates price data for a fixed order's price
///
/// Reverse of fixed_price_lots()
pub fn fixed_price_data(price_lots: i64) -> Result<u64, ProgramError> {
    if price_lots < 1 {
        return Err(ProgramError::InvalidArgument);
    }
    Ok(price_lots as u64)
}

/// Retrieves the price (in lots) from a fixed order's price data
///
/// Reverse of fixed_price_data().
pub fn fixed_price_lots(price_data: u64) -> i64 {
    assert!(price_data <= i64::MAX as u64);
    price_data as i64
}

/// InnerNodes and LeafNodes compose the binary tree of orders.
///
/// Each InnerNode has exactly two children, which are either InnerNodes themselves,
/// or LeafNodes. The children share the top `prefix_len` bits of `key`. The left
/// child has a 0 in the next bit, and the right a 1.
#[derive(Copy, Clone, BorshDeserialize, BorshSerialize)]
#[repr(C)]
#[borsh(crate = "borsh")]
pub struct InnerNode {
    pub tag: u8, // NodeTag
    pub padding: [u8; 3],
    /// number of highest `key` bits that all children share
    /// e.g. if it's 2, the two highest bits of `key` will be the same on all children
    pub prefix_len: u32,

    /// only the top `prefix_len` bits of `key` are relevant
    pub key: u128,

    /// indexes into `BookSide::nodes`
    pub children: [NodeHandle; 2],

    /// The earliest expiry timestamp for the left and right subtrees.
    ///
    /// Needed to be able to find and remove expired orders without having to
    /// iterate through the whole bookside.
    pub child_earliest_expiry: [u64; 2],

    pub reserved: [u8; 72],
}
unsafe impl bytemuck::Pod for InnerNode {}
unsafe impl bytemuck::Zeroable for InnerNode {}
const_assert_eq!(size_of::<InnerNode>(), 4 + 4 + 16 + 4 * 2 + 8 * 2 + 72);
const_assert_eq!(size_of::<InnerNode>(), NODE_SIZE);
const_assert_eq!(size_of::<InnerNode>() % 8, 0);

impl InnerNode {
    pub fn new(prefix_len: u32, key: u128) -> Self {
        Self {
            tag: NodeTag::InnerNode.into(),
            padding: Default::default(),
            prefix_len,
            key,
            children: [0; 2],
            child_earliest_expiry: [u64::MAX; 2],
            reserved: [0; NODE_SIZE - 48],
        }
    }

    /// Returns the handle of the child that may contain the search key
    /// and 0 or 1 depending on which child it was.
    pub(crate) fn walk_down(&self, search_key: u128) -> (NodeHandle, bool) {
        let crit_bit_mask = 1u128 << (127 - self.prefix_len);
        let crit_bit = (search_key & crit_bit_mask) != 0;
        (self.children[crit_bit as usize], crit_bit)
    }

    /// The lowest timestamp at which one of the contained LeafNodes expires.
    #[inline(always)]
    pub fn earliest_expiry(&self) -> u64 {
        std::cmp::min(self.child_earliest_expiry[0], self.child_earliest_expiry[1])
    }
}

/// LeafNodes represent an order in the binary tree
#[derive(Debug, Copy, Clone, PartialEq, Eq, BorshDeserialize, BorshSerialize)]
#[repr(C)]
#[borsh(crate = "borsh")]
pub struct LeafNode {
    /// NodeTag
    pub tag: u8,

    /// Index into the owning MangoAccount's PerpOpenOrders
    /// pub owner_slot: u8,

    /// PostOrderType, this was added for TradingView move order
    pub order_type: u8,

    pub padding: [u8; 2],

    /// Time in seconds after `timestamp` at which the order expires.
    /// A value of 0 means no expiry.
    pub time_in_force: u16,

    pub padding2: [u8; 2],

    /// The binary tree key, see new_node_key()
    pub key: u128,

    /// Address of the owning MangoAccount
    pub owner: Pubkey,

    /// Number of base lots to buy or sell, always >=1
    pub quantity: i64,

    /// The time the order was placed
    pub timestamp: u64,

    /// User defined id for this order, used in FillEvents
    pub client_order_id: u64,

    pub reserved: [u8; 40],
}
unsafe impl bytemuck::Pod for LeafNode {}
unsafe impl bytemuck::Zeroable for LeafNode {}
const_assert_eq!(
    size_of::<LeafNode>(),
    4 + 2 + 1 + 1 + 16 + 32 + 8 + 8 + 8 + 40
);
const_assert_eq!(size_of::<LeafNode>(), NODE_SIZE);
const_assert_eq!(size_of::<LeafNode>() % 8, 0);

impl LeafNode {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key: u128,
        owner: Pubkey,
        quantity: i64,
        timestamp: u64,
        order_type: PostOrderType,
        time_in_force: u16,
        client_order_id: u64,
    ) -> Self {
        Self {
            tag: NodeTag::LeafNode.into(),
            order_type: order_type.into(),
            padding: Default::default(),
            time_in_force,
            padding2: Default::default(),
            key,
            owner,
            quantity,
            timestamp,
            client_order_id,
            reserved: [0; 40],
        }
    }

    /// The order's price_data as stored in the key
    ///
    /// Needs to be unpacked differently for fixed and oracle pegged orders.
    #[inline(always)]
    pub fn price_data(&self) -> u64 {
        (self.key >> 64) as u64
    }

    /// Time at which this order will expire, u64::MAX if never
    #[inline(always)]
    pub fn expiry(&self) -> u64 {
        if self.time_in_force == 0 {
            u64::MAX
        } else {
            self.timestamp + self.time_in_force as u64
        }
    }

    /// Returns if the order is expired at `now_ts`
    #[inline(always)]
    pub fn is_expired(&self, now_ts: u64) -> bool {
        self.time_in_force > 0 && now_ts >= self.timestamp + self.time_in_force as u64
    }
}

#[derive(Copy, Clone)]
#[repr(C)]
pub struct FreeNode {
    pub(crate) tag: u8, // NodeTag
    pub(crate) padding: [u8; 3],
    pub(crate) next: NodeHandle,
    pub(crate) reserved: [u8; NODE_SIZE - 16],
    // ensure that FreeNode has the same 8-byte alignment as other nodes
    pub(crate) force_align: u64,
}
unsafe impl bytemuck::Pod for FreeNode {}
unsafe impl bytemuck::Zeroable for FreeNode {}
const_assert_eq!(size_of::<FreeNode>(), NODE_SIZE);
const_assert_eq!(size_of::<FreeNode>() % 8, 0);

#[derive(Copy, Clone, BorshDeserialize, BorshSerialize)]
#[borsh(crate = "borsh")]
#[repr(C)]
pub struct AnyNode {
    pub tag: u8,
    pub data: [u8; 111],
    // ensure that AnyNode has the same 8-byte alignment as other nodes
    pub(crate) force_align: u64,
}
unsafe impl bytemuck::Pod for AnyNode {}
unsafe impl bytemuck::Zeroable for AnyNode {}
const_assert_eq!(size_of::<AnyNode>(), NODE_SIZE);
const_assert_eq!(size_of::<AnyNode>() % 8, 0);
const_assert_eq!(size_of::<AnyNode>(), size_of::<InnerNode>());
const_assert_eq!(size_of::<AnyNode>(), size_of::<LeafNode>());
const_assert_eq!(size_of::<AnyNode>(), size_of::<FreeNode>());
const_assert_eq!(align_of::<AnyNode>(), 8);
const_assert_eq!(align_of::<AnyNode>(), align_of::<InnerNode>());
const_assert_eq!(align_of::<AnyNode>(), align_of::<LeafNode>());
const_assert_eq!(align_of::<AnyNode>(), align_of::<FreeNode>());

pub(crate) enum NodeRef<'a> {
    Inner(&'a InnerNode),
    Leaf(&'a LeafNode),
}

pub(crate) enum NodeRefMut<'a> {
    Inner(&'a mut InnerNode),
    Leaf(&'a mut LeafNode),
}

impl AnyNode {
    pub fn key(&self) -> Option<u128> {
        match self.case()? {
            NodeRef::Inner(inner) => Some(inner.key),
            NodeRef::Leaf(leaf) => Some(leaf.key),
        }
    }

    pub(crate) fn children(&self) -> Option<[NodeHandle; 2]> {
        match self.case().unwrap() {
            NodeRef::Inner(&InnerNode { children, .. }) => Some(children),
            NodeRef::Leaf(_) => None,
        }
    }

    pub(crate) fn case(&self) -> Option<NodeRef> {
        match NodeTag::try_from(self.tag) {
            Ok(NodeTag::InnerNode) => Some(NodeRef::Inner(cast_ref(self))),
            Ok(NodeTag::LeafNode) => Some(NodeRef::Leaf(cast_ref(self))),
            _ => None,
        }
    }

    fn case_mut(&mut self) -> Option<NodeRefMut> {
        match NodeTag::try_from(self.tag) {
            Ok(NodeTag::InnerNode) => Some(NodeRefMut::Inner(cast_mut(self))),
            Ok(NodeTag::LeafNode) => Some(NodeRefMut::Leaf(cast_mut(self))),
            _ => None,
        }
    }

    #[inline]
    pub fn as_leaf(&self) -> Option<&LeafNode> {
        match self.case() {
            Some(NodeRef::Leaf(leaf_ref)) => Some(leaf_ref),
            _ => None,
        }
    }

    #[inline]
    pub fn as_leaf_mut(&mut self) -> Option<&mut LeafNode> {
        match self.case_mut() {
            Some(NodeRefMut::Leaf(leaf_ref)) => Some(leaf_ref),
            _ => None,
        }
    }

    #[inline]
    pub fn as_inner(&self) -> Option<&InnerNode> {
        match self.case() {
            Some(NodeRef::Inner(inner_ref)) => Some(inner_ref),
            _ => None,
        }
    }

    #[inline]
    pub fn as_inner_mut(&mut self) -> Option<&mut InnerNode> {
        match self.case_mut() {
            Some(NodeRefMut::Inner(inner_ref)) => Some(inner_ref),
            _ => None,
        }
    }

    #[inline]
    pub fn earliest_expiry(&self) -> u64 {
        match self.case().unwrap() {
            NodeRef::Inner(inner) => inner.earliest_expiry(),
            NodeRef::Leaf(leaf) => leaf.expiry(),
        }
    }
}

impl AsRef<AnyNode> for InnerNode {
    fn as_ref(&self) -> &AnyNode {
        cast_ref(self)
    }
}

impl AsRef<AnyNode> for LeafNode {
    #[inline]
    fn as_ref(&self) -> &AnyNode {
        cast_ref(self)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshDeserialize;
    use solana_program::pubkey::Pubkey;

    #[test]
    fn test_borsh_serialization_inner_node() {
        let inner_node = InnerNode::new(5, 12345);
        let mut serialized_data = Vec::new();
        inner_node
            .serialize(&mut serialized_data)
            .expect("Inner node serialization failed");
        let deserialized: InnerNode = InnerNode::try_from_slice(&serialized_data).unwrap();
        assert_eq!(inner_node.tag, deserialized.tag);
        assert_eq!(inner_node.prefix_len, deserialized.prefix_len);
        assert_eq!(inner_node.key, deserialized.key);
        assert_eq!(inner_node.children, deserialized.children);
    }

    #[test]
    fn test_borsh_serialization_leaf_node() {
        let owner = Pubkey::new_unique();
        let leaf_node = LeafNode::new(
            12345,
            owner,
            100,
            1618884738,
            PostOrderType::Limit,
            300,
            56789,
        );
        let mut serialized_data = Vec::new();
        leaf_node
            .serialize(&mut serialized_data)
            .expect("Leaf node serialization failed");
        let deserialized: LeafNode = LeafNode::try_from_slice(&serialized_data).unwrap();
        assert_eq!(leaf_node.tag, deserialized.tag);
        assert_eq!(leaf_node.order_type, deserialized.order_type);
        assert_eq!(leaf_node.key, deserialized.key);
        assert_eq!(leaf_node.owner, deserialized.owner);
        assert_eq!(leaf_node.quantity, deserialized.quantity);
        assert_eq!(leaf_node.timestamp, deserialized.timestamp);
        assert_eq!(leaf_node.client_order_id, deserialized.client_order_id);
    }

    #[test]
    fn test_any_node_conversion() {
        let owner = Pubkey::new_unique();
        let leaf_node = LeafNode::new(
            12345,
            owner,
            100,
            1618884738,
            PostOrderType::Limit,
            300,
            56789,
        );
        let inner_node = InnerNode::new(5, 12345);

        let any_leaf_node: &AnyNode = leaf_node.as_ref();
        let leaf_node_tag: u8 = NodeTag::LeafNode.into();
        assert_eq!(any_leaf_node.tag, leaf_node_tag);

        let any_inner_node: &AnyNode = inner_node.as_ref();
        let inner_node_tag: u8 = NodeTag::InnerNode.into();
        assert_eq!(any_inner_node.tag, inner_node_tag);

        let mut leaf_node_serialized_data = Vec::new();
        any_leaf_node
            .serialize(&mut leaf_node_serialized_data)
            .expect("Leaf node serialization failed");
        let deserialized_any_leaf: AnyNode =
            AnyNode::try_from_slice(&leaf_node_serialized_data).unwrap();
        let mut any_node_serialized_data = Vec::new();
        any_inner_node
            .serialize(&mut any_node_serialized_data)
            .expect("Any node serialization failed");
        let deserialized_any_inner: AnyNode =
            AnyNode::try_from_slice(&any_node_serialized_data).unwrap();

        let leaf_node_tag: u8 = NodeTag::LeafNode.into();
        assert_eq!(deserialized_any_leaf.tag, leaf_node_tag);
        let inner_node_tag: u8 = NodeTag::InnerNode.into();
        assert_eq!(deserialized_any_inner.tag, inner_node_tag);

        if let Some(NodeRef::Leaf(leaf_ref)) = deserialized_any_leaf.case() {
            assert_eq!(leaf_ref.key, leaf_node.key);
            assert_eq!(leaf_ref.owner, leaf_node.owner);
        } else {
            panic!("Deserialized AnyNode is not a LeafNode");
        }

        if let Some(NodeRef::Inner(inner_ref)) = deserialized_any_inner.case() {
            assert_eq!(inner_ref.key, inner_node.key);
            assert_eq!(inner_ref.prefix_len, inner_node.prefix_len);
        } else {
            panic!("Deserialized AnyNode is not an InnerNode");
        }
    }
}
