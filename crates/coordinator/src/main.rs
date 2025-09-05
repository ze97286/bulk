use bulk_coordinator::Coordinator;

#[tokio::main]
async fn main() {
    println!("Bulk Order Propagation System - Async Tasks\n");

    let node_count = 1000;

    let mut coordinator = Coordinator::new(node_count);

    // Set up the network
    coordinator.setup_network().await;

    // Inject a new order
    coordinator.inject_order(0).await;

    // Wait for the order to be propagated - scale the wait by the network size
    let wait_time = if node_count > 100 { 3 } else { 1 };
    coordinator.wait_and_measure(wait_time).await;

    println!("Simulation complete");
}
