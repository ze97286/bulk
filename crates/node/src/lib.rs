use bulk_types::{GossipPacket, Order};
use bytes::Bytes;
use smallvec::SmallVec;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

pub type NodeId = u16;
pub type NodeSender = mpsc::UnboundedSender<Arc<GossipPacket>>;

const FANOUT: usize = 8;

pub struct NodeProcess {
    pub id: NodeId,
    pub sender: NodeSender,
    pub receiver: mpsc::UnboundedReceiver<Arc<GossipPacket>>,
    pub peers: Vec<NodeId>,
    pub peer_senders: Vec<NodeSender>,
    seen: AtomicU64,
    pub first_latency: Arc<OnceLock<u64>>,
    pub nodes_reached: Arc<AtomicUsize>,
    pub notify: Arc<tokio::sync::Notify>,
}

impl NodeProcess {
    pub fn new(
        id: NodeId,
        peers: Vec<NodeId>,
        nodes_reached: Arc<AtomicUsize>,
        notify: Arc<tokio::sync::Notify>,
    ) -> (Self, NodeSender) {
        let (sender, receiver) = mpsc::unbounded_channel();

        let node = Self {
            id,
            sender: sender.clone(),
            receiver,
            peers,
            peer_senders: Vec::new(),
            seen: AtomicU64::new(0),
            first_latency: Arc::new(OnceLock::new()),
            nodes_reached,
            notify,
        };

        (node, sender)
    }

    pub fn set_peer_senders(&mut self, senders: Vec<NodeSender>) {
        self.peer_senders = senders;
    }

    /// Process a new order - called only on first receipt
    fn process_order(&self, order: &Order) {
        // placeholder for doing something with the order
        let _ = order;
    }

    pub async fn run(mut self) {
        while let Some(packet) = self.receiver.recv().await {
            self.handle_packet(packet).await;
        }
    }

    async fn handle_packet(&self, packet: Arc<GossipPacket>) {
        let parsed: Order = postcard::from_bytes(&packet.serialized).unwrap();
        let id = parsed.order_id;
        if self.seen.load(Ordering::Relaxed) == id {
            return;
        }
        if self
            .seen
            .compare_exchange(0, id, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        // First time this node sees the order - process it
        self.process_order(&parsed);

        let elapsed = packet.t0.elapsed().as_nanos() as u64;
        if self.first_latency.set(elapsed).is_ok() {
            let _count = self.nodes_reached.fetch_add(1, Ordering::Relaxed) + 1;
            self.notify.notify_one();
        }

        if packet.ttl > 0 {
            self.forward_to_peers(parsed, packet).await;
        }
    }

    async fn forward_to_peers(&self, parsed: Order, prev: Arc<GossipPacket>) {
        if self.peer_senders.is_empty() || prev.ttl == 0 {
            return;
        }

        // encode once for this hop
        let next_bytes = Arc::new(Bytes::from(postcard::to_stdvec(&parsed).unwrap()));
        let next = Arc::new(GossipPacket {
            order: Arc::new(parsed),
            serialized: next_bytes,
            ttl: prev.ttl - 1,
            source_node: prev.source_node,
            t0: prev.t0,
        });

        // choose FANOUT unique peers
        let len = self.peer_senders.len();
        let fanout = FANOUT.min(len);
        let mut idx: SmallVec<[usize; 32]> = (0..len).collect();
        for i in 0..fanout {
            let j = i + fastrand::usize(..(len - i));
            idx.swap(i, j);
            let _ = self.peer_senders[idx[i]].send(next.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bulk_types::{MarketId, Order, OrderType, Side, TimeInForce};
    use std::time::Duration;
    use tokio::time::sleep;

    fn create_test_order(order_id: u64) -> Order {
        Order {
            order_id,
            client_order_id: format!("test-{}", order_id),
            user: alloy_primitives::Address::ZERO,
            side: Side::Buy,
            price: 100_000_000_000u128,
            size: 1_000_000_000u128,
            market_id: MarketId {
                base_token: alloy_primitives::Address::ZERO,
                quote_token: alloy_primitives::Address::ZERO,
            },
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GoodTilCanceled,
            timestamp: 0,
        }
    }

    fn create_test_packet(order_id: u64, ttl: u8) -> Arc<GossipPacket> {
        let order = create_test_order(order_id);
        let order_arc = Arc::new(order);
        let serialized = Arc::new(Bytes::from(postcard::to_stdvec(&*order_arc).unwrap()));
        Arc::new(GossipPacket {
            order: order_arc,
            serialized,
            ttl,
            source_node: 0,
            t0: std::time::Instant::now(),
        })
    }

    #[tokio::test]
    async fn test_node_creation() {
        let nodes_reached = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());
        let peers = vec![1, 2, 3];

        let (node, sender) = NodeProcess::new(0, peers.clone(), nodes_reached, notify);

        assert_eq!(node.id, 0);
        assert_eq!(node.peers, peers);
        assert!(sender.send(create_test_packet(1, 5)).is_ok());
    }

    #[tokio::test]
    async fn test_deduplication() {
        let nodes_reached = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());

        let (node, _sender) = NodeProcess::new(0, vec![], nodes_reached.clone(), notify.clone());

        // Send same order twice
        let packet = create_test_packet(42, 5);

        // Process first packet
        node.handle_packet(packet.clone()).await;
        assert_eq!(nodes_reached.load(Ordering::Relaxed), 1);

        // Process duplicate - should be ignored
        node.handle_packet(packet).await;
        assert_eq!(nodes_reached.load(Ordering::Relaxed), 1); // Still 1
    }

    #[tokio::test]
    async fn test_ttl_decrement() {
        let nodes_reached = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());

        // Create two nodes
        let (mut node1, _sender1) =
            NodeProcess::new(1, vec![2], nodes_reached.clone(), notify.clone());
        let (node2, sender2) =
            NodeProcess::new(2, vec![], nodes_reached.clone(), notify.clone());

        // Connect nodes
        node1.set_peer_senders(vec![sender2.clone()]);

        // Spawn node2 to receive messages
        let handle = tokio::spawn(async move {
            let mut node2 = node2;
            if let Some(packet) = node2.receiver.recv().await {
                assert_eq!(packet.ttl, 4); // Should be decremented from 5 to 4
                packet
            } else {
                panic!("Expected packet");
            }
        });

        // Send packet with TTL=5 through node1
        let packet = create_test_packet(1, 5);
        node1.handle_packet(packet).await;

        // Verify TTL was decremented
        let received = handle.await.unwrap();
        assert_eq!(received.ttl, 4);
    }

    #[tokio::test]
    async fn test_ttl_zero_no_forward() {
        let nodes_reached = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());

        let (mut node1, _sender1) =
            NodeProcess::new(1, vec![2], nodes_reached.clone(), notify.clone());
        let (mut node2, sender2) =
            NodeProcess::new(2, vec![], nodes_reached.clone(), notify.clone());

        node1.set_peer_senders(vec![sender2.clone()]);

        // Send packet with TTL=0 - should not forward
        let packet = create_test_packet(1, 0);
        node1.handle_packet(packet).await;

        // Give time for any potential forward
        sleep(Duration::from_millis(10)).await;

        // Node2 should not receive anything
        assert!(node2.receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_first_latency_recording() {
        let nodes_reached = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());

        let (node, _sender) = NodeProcess::new(0, vec![], nodes_reached, notify);

        // Process packet
        let packet = create_test_packet(1, 5);
        node.handle_packet(packet).await;

        // Check latency was recorded
        assert!(node.first_latency.get().is_some());
        let latency = *node.first_latency.get().unwrap();
        assert!(latency > 0); // Should have some non-zero latency
    }

    #[tokio::test]
    async fn test_fanout_limit() {
        let nodes_reached = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());

        // Create node with many peers
        let peers: Vec<u16> = (1..=20).collect();
        let (mut node, _sender) =
            NodeProcess::new(0, peers.clone(), nodes_reached.clone(), notify.clone());

        // Create peer senders
        let mut peer_senders = Vec::new();
        let mut peer_receivers = Vec::new();
        for i in 1..=20 {
            let (peer_node, peer_sender) =
                NodeProcess::new(i, vec![], nodes_reached.clone(), notify.clone());
            peer_senders.push(peer_sender);
            peer_receivers.push(peer_node.receiver);
        }

        node.set_peer_senders(peer_senders);

        // Send packet
        let packet = create_test_packet(1, 5);
        node.handle_packet(packet).await;

        // Count how many peers received the message
        sleep(Duration::from_millis(10)).await;
        let mut forwarded_count = 0;
        for mut receiver in peer_receivers {
            if receiver.try_recv().is_ok() {
                forwarded_count += 1;
            }
        }

        // Should forward to exactly FANOUT peers
        assert_eq!(forwarded_count, FANOUT);
    }

    #[tokio::test]
    async fn test_serialization_deserialization() {
        let nodes_reached = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());

        let (mut node1, _sender1) =
            NodeProcess::new(1, vec![2], nodes_reached.clone(), notify.clone());
        let (node2, sender2) =
            NodeProcess::new(2, vec![], nodes_reached.clone(), notify.clone());

        node1.set_peer_senders(vec![sender2]);

        // Spawn receiver
        let handle = tokio::spawn(async move {
            let mut node2 = node2;
            if let Some(packet) = node2.receiver.recv().await {
                // Verify we can deserialize the forwarded packet
                let order: Order = postcard::from_bytes(&packet.serialized).unwrap();
                assert_eq!(order.order_id, 99);
                assert_eq!(order.client_order_id, "test-99");
            } else {
                panic!("Expected packet");
            }
        });

        // Send packet with specific order details
        let packet = create_test_packet(99, 5);
        node1.handle_packet(packet).await;

        // Wait for assertion in spawned task
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_notify_on_first_seen() {
        let nodes_reached = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());

        let (node, _sender) = NodeProcess::new(0, vec![], nodes_reached.clone(), notify.clone());

        // Process packet
        let packet = create_test_packet(1, 5);

        // Start waiting for notification
        let notify_clone = notify.clone();
        let handle = tokio::spawn(async move {
            notify_clone.notified().await;
            true
        });

        node.handle_packet(packet).await;

        // Should receive notification
        let notified = tokio::time::timeout(Duration::from_millis(100), handle)
            .await
            .unwrap()
            .unwrap();
        assert!(notified);
    }

    #[tokio::test]
    async fn test_concurrent_deduplication() {
        let nodes_reached = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(tokio::sync::Notify::new());

        let (node, _sender) = NodeProcess::new(0, vec![], nodes_reached.clone(), notify);
        let node = Arc::new(node);

        // Try to process same order from multiple tasks concurrently
        let mut handles = vec![];
        for _ in 0..10 {
            let node_clone = node.clone();
            let packet = create_test_packet(42, 5);
            let handle = tokio::spawn(async move {
                node_clone.handle_packet(packet).await;
            });
            handles.push(handle);
        }

        // Wait for all to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // Only one should have succeeded
        assert_eq!(nodes_reached.load(Ordering::Relaxed), 1);
    }
}
