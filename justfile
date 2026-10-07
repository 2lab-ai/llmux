# llmux task runner (herdr conventions)

# Format + lint + test — the gate before every commit
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Run tests
test:
    cargo test

# Build release binary
build:
    cargo build --release --locked

# Format the tree
fmt:
    cargo fmt

# Agent SDK contracts (install locked deps separately with `cd bridge && npm ci --ignore-scripts`)
check-bridge:
    cd bridge && npm test
