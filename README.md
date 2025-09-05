# Bulk Order Propagation - Gossip Protocol Simulation

A multi-process UDP-based gossip protocol simulation for order propagation.

## Usage

```bash
# Build and run simulation with default settings (10 nodes)
just run
```

## Example Run

```
Bulk Order Propagation System - Async Tasks

Setting up network with 1000 nodes...
Network setup complete - Average peers per node: 20.0

Injecting order into node 0 with TTL 6
Monitoring propagation (max 3 seconds)...
95% coverage (950/1000) reached at 755.917 μs
Stopped gossip propagation at 95% coverage

=== RESULTS (Captured at 95% Coverage) ===
Total nodes reached: 950/1000
Time to 95% coverage: 755.917 μs

Latency Distribution:
   0.000-  92.954 μs:  95 nodes
  92.954- 185.908 μs: 229 nodes
 185.908- 278.862 μs: 191 nodes
 278.862- 371.816 μs: 149 nodes
 371.816- 464.770 μs: 115 nodes
 464.770- 557.724 μs:  74 nodes
 557.724- 650.678 μs:  55 nodes
 650.678- 743.632 μs:  39 nodes
 743.632- 836.586 μs:  24 nodes
 836.586- 929.540 μs:  13 nodes
Simulation complete
```

## Main design decisions and rationale

- **Serialization**: Using postcard for its compact binary format (smallest encoded size in benchmarks) and zero-copy deserialization support, prioritizing network bandwidth over raw CPU serialization speed - the idea is that for the given that messages fan out expoentially across peers it's more important than raw ser/de speed. See the benchmark in https://github.com/djkoloski/rust_serialization_benchmark. 

- **Network Topology**: Erdős–Rényi random graph with degree-based connectivity - following the general idea from libp2p GossipSub's of using random peer selection to ensure robust message delivery even with node failures, while maintaining logarithmic hop counts for network-wide propagation.

- **Gossip Protocol**: Message propagation with fan out = 8 (as required, though should be more dependent on the network size), and TTL = 6 to prevent loops (again, TTL should depend on the FANOUT).  Each node forwards to a random subset of peers (rather than flooding all), achieving 95% network coverage in O(log N) hops while minimizing redundant message transmission, balancing propagation speed and network efficiency
   
- **Additional Implementation Details for Performance**:
  - Lock-free deduplication using AtomicU64 for single-order scenarios, eliminating async lock overhead in the hot path
  - Zero-copy message sharing with Arc<Order> within the process while still simulating serialization costs
  - SmallVec-based partial shuffle for fanout selection, avoiding heap allocations when selecting 8 peers from up to 32
  - Real-time 95% detection using atomic counters with Notify, stopping propagation immediately at target coverage instead of waiting for 100%
