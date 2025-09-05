use alloy_primitives::Address;
use bulk_node::{NodeProcess, NodeSender};
use bulk_types::{GossipPacket, MarketId, Order, OrderType, Side, TimeInForce};
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

pub struct Coordinator {
    node_count: u16,
    latencies: Vec<Arc<std::sync::OnceLock<u64>>>,
    node_handles: Vec<JoinHandle<()>>,
    node_senders: HashMap<u16, NodeSender>,
    nodes_reached: Arc<AtomicUsize>,
    notify: Arc<tokio::sync::Notify>,
}

impl Coordinator {
    pub fn new(node_count: u16) -> Self {
        Self {
            node_count,
            latencies: Vec::with_capacity(node_count as usize),
            node_handles: Vec::new(),
            node_senders: HashMap::new(),
            nodes_reached: Arc::new(AtomicUsize::new(0)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub async fn setup_network(&mut self) {
        println!("Setting up network with {} nodes...", self.node_count);

        let mut all_nodes = HashMap::new();

        for node_id in 0..self.node_count {
            let peers = self.generate_peers(node_id);
            let (node, sender) = NodeProcess::new(
                node_id,
                peers.clone(),
                self.nodes_reached.clone(),
                self.notify.clone(),
            );

            self.node_senders.insert(node_id, sender);
            self.latencies.push(node.first_latency.clone());
            all_nodes.insert(node_id, node);
        }

        for node in all_nodes.values_mut() {
            let mut peer_senders = Vec::new();
            for &peer_id in &node.peers {
                if let Some(peer_sender) = self.node_senders.get(&peer_id) {
                    peer_senders.push(peer_sender.clone());
                }
            }
            node.set_peer_senders(peer_senders);
        }

        let total_peers: usize = all_nodes.values().map(|n| n.peers.len()).sum();
        let avg_peers = total_peers as f64 / self.node_count as f64;
        println!(
            "Network setup complete - Average peers per node: {:.1}",
            avg_peers
        );

        for (_node_id, node) in all_nodes {
            let handle = tokio::spawn(async move {
                node.run().await;
            });
            self.node_handles.push(handle);
        }
    }

    fn generate_peers(&self, node_id: u16) -> Vec<u16> {
        let degree = if self.node_count >= 1000 {
            20
        } else if self.node_count > 100 {
            12
        } else {
            6
        };

        let mut peers = Vec::with_capacity(degree);

        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        node_id.hash(&mut hasher);
        let mut rng_state = hasher.finish();

        let mut attempts = 0;
        while peers.len() < degree && attempts < degree * 10 {
            attempts += 1;

            rng_state = rng_state.wrapping_mul(1664525).wrapping_add(1013904223);
            let random_node = (rng_state % self.node_count as u64) as u16;

            if random_node != node_id && !peers.contains(&random_node) {
                peers.push(random_node);
            }
        }

        peers.sort();
        peers
    }

    pub async fn inject_order(&self, target_node: u16) {
        // TTL calculation: ceil(log_fanout(N)) + safety margin
        // For fanout=8: log_8(1000) ≈ 3.3, so ceil = 4, plus margin = 6
        // With TTL=6, messages can reach nodes up to 6 hops away, which is
        // sufficient for >95% coverage in a random graph with average degree ~20
        let ttl = 6;
        let t0 = Instant::now();

        let order = Order {
            order_id: 1,
            client_order_id: "test-order".to_string(),
            user: Address::ZERO,
            side: Side::Buy,
            price: 100_000_000_000u128,
            size: 1_000_000_000u128,
            market_id: MarketId {
                base_token: Address::ZERO,
                quote_token: Address::ZERO,
            },
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GoodTilCanceled,
            timestamp: t0.elapsed().as_nanos() as u64,
        };

        let order_arc = Arc::new(order);
        let serialized = Arc::new(Bytes::from(postcard::to_stdvec(&*order_arc).unwrap()));

        let packet = Arc::new(GossipPacket {
            order: order_arc,
            serialized,
            ttl,
            source_node: target_node,
            t0,
        });

        if let Some(sender) = self.node_senders.get(&target_node) {
            println!(
                "\nInjecting order into node {} with TTL {}",
                target_node, ttl
            );
            let _ = sender.send(packet);
        }
    }

    pub async fn wait_and_measure(&self, duration_secs: u64) {
        println!("Monitoring propagation (max {} seconds)...", duration_secs);

        let target_95_nodes = (self.node_count as f64 * 0.95) as usize;
        let mut final_metrics = None;

        let timeout_duration = Duration::from_secs(duration_secs);
        let start = Instant::now();

        while start.elapsed() < timeout_duration {
            let nodes_reached = self.nodes_reached.load(Ordering::Relaxed);

            if nodes_reached >= target_95_nodes && final_metrics.is_none() {
                let mut times = Vec::with_capacity(self.node_count as usize);
                for latency_lock in &self.latencies {
                    if let Some(&t) = latency_lock.get() {
                        times.push(t);
                    }
                }

                times.sort_unstable();
                let time_to_95_nanos = if times.len() >= target_95_nodes {
                    times[target_95_nodes - 1]
                } else {
                    times.last().copied().unwrap_or(0)
                };

                println!(
                    "95% coverage ({}/{}) reached at {:.3} μs",
                    target_95_nodes,
                    self.node_count,
                    time_to_95_nanos as f64 / 1_000.0
                );

                final_metrics = Some((times, target_95_nodes, time_to_95_nanos));

                for handle in &self.node_handles {
                    handle.abort();
                }
                println!("Stopped gossip propagation at 95% coverage");
                break;
            }

            self.notify.notified().await;
        }

        if let Some((times, nodes_reached, time_to_95)) = final_metrics {
            self.analyze_95_results(times, nodes_reached, time_to_95)
                .await;
        } else {
            self.analyze_results().await;
        }
    }

    async fn analyze_95_results(&self, times: Vec<u64>, nodes_reached: usize, time_to_95: u64) {
        println!("\n=== RESULTS (Captured at 95% Coverage) ===");

        println!("Total nodes reached: {}/{}", nodes_reached, self.node_count);
        println!(
            "Time to 95% coverage: {:.3} μs",
            time_to_95 as f64 / 1_000.0
        );

        if !times.is_empty() {
            println!("\nLatency Distribution:");
            let max_latency = *times.last().unwrap();
            let bucket_size_nanos = (max_latency / 10).max(1);
            let mut histogram = [0; 10];

            for &latency in &times {
                let bucket = (latency / bucket_size_nanos) as usize;
                let bucket = bucket.min(9);
                histogram[bucket] += 1;
            }

            for (i, count) in histogram.iter().enumerate() {
                let start_micros = (i as u64 * bucket_size_nanos) as f64 / 1_000.0;
                let end_micros = ((i + 1) as u64 * bucket_size_nanos) as f64 / 1_000.0;
                println!(
                    "{:8.3}-{:8.3} μs: {:3} nodes",
                    start_micros, end_micros, count
                );
            }
        } else {
            println!("No metrics collected - check network connectivity");
        }
    }

    async fn analyze_results(&self) {
        println!("\n=== RESULTS ===");

        let mut times = Vec::new();

        for latency_lock in &self.latencies {
            if let Some(&t) = latency_lock.get() {
                times.push(t);
            }
        }

        println!("Total nodes reached: {}/{}", times.len(), self.node_count);

        if !times.is_empty() {
            times.sort_unstable();

            let target_95_nodes = (self.node_count as f64 * 0.95) as usize;

            if times.len() >= target_95_nodes {
                let idx_95 = target_95_nodes - 1;
                let time_to_95_nanos = times[idx_95];
                let time_to_95_micros = time_to_95_nanos as f64 / 1_000.0;
                println!("Time to 95% coverage: {:.3} μs", time_to_95_micros);
            } else {
                println!(
                    "95% coverage NOT achieved!!! - only reached {}/{} nodes ({:.1}%)",
                    times.len(),
                    self.node_count,
                    times.len() as f64 / self.node_count as f64 * 100.0
                );
            }

            println!("\nLatency Distribution:");
            let max_latency = *times.last().unwrap();
            let bucket_size_nanos = (max_latency / 10).max(1);
            let mut histogram = [0; 10];

            for &latency in &times {
                let bucket = (latency / bucket_size_nanos) as usize;
                let bucket = bucket.min(9);
                histogram[bucket] += 1;
            }

            for (i, count) in histogram.iter().enumerate() {
                let start_micros = (i as u64 * bucket_size_nanos) as f64 / 1_000.0;
                let end_micros = ((i + 1) as u64 * bucket_size_nanos) as f64 / 1_000.0;
                println!(
                    "{:8.3}-{:8.3} μs: {:3} nodes",
                    start_micros, end_micros, count
                );
            }
        } else {
            println!("No metrics collected - check network connectivity");
        }
    }
}
