# Bulk Order Propagation - Just Commands

# Default recipe that lists all available commands
default:
    @just --list

# Build all crates
build:
    cargo build --workspace

# Build in release mode
build-release:
    cargo build --workspace --release

# Run the coordinator (simulation)
run:
    just build-release && ./target/release/bulk-coordinator

# Run a single node (for testing)
run-node id port *peers:
    just build-release && ./target/release/bulk-node {{id}} {{port}} {{peers}}

# Check all code
check:
    cargo check --workspace

# Run clippy on all crates
clippy:
    cargo clippy --workspace -- -D warnings

# Format all code
fmt:
    cargo fmt --all

# Check formatting
fmt-check:
    cargo fmt --all --check

# Run all tests
test:
    cargo test --workspace

# Clean build artifacts
clean:
    cargo clean

# Run lint (clippy + fmt check)
lint:
    just clippy
    just fmt-check

# Full CI pipeline
ci: lint test build

# Show crate structure
tree:
    find crates -name "*.toml" -o -name "*.rs" | sort