use alloy_primitives::Address;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum TimeInForce {
    GoodTilCanceled = 0,
    PostOnly = 1,
    FillOrKill = 2,
    ImmediateOrCancel = 3,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum Side {
    Buy = 0,
    Sell = 1,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum OrderType {
    Limit = 0,
    Market = 1,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MarketId {
    pub base_token: Address,
    pub quote_token: Address,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Order {
    pub order_id: u64,
    pub client_order_id: String,
    pub user: Address,
    pub side: Side,
    pub price: u128,
    pub size: u128,
    pub market_id: MarketId,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub timestamp: u64,
}

#[derive(Clone, Debug)]
pub struct GossipPacket {
    pub order: Arc<Order>,
    pub serialized: Arc<bytes::Bytes>,
    pub ttl: u8,
    pub source_node: u16,
    pub t0: std::time::Instant,
}
