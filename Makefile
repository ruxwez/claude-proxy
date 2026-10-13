CARGO ?= cargo
ENDPOINT ?= http://127.0.0.1:10801
.PHONY: build run cli install-cli update-models preview-models test fmt lint check docker-up docker-down
build:
	$(CARGO) build --workspace --release --locked
run:
	$(CARGO) run --locked -p claude-proxy-server
cli:
	$(CARGO) run --locked -p claude-proxy-cli
install-cli:
	$(CARGO) install --locked --path crates/cli
update-models:
	$(CARGO) run --locked -p claude-proxy-cli -- --sync --endpoint "$(ENDPOINT)"
preview-models:
	$(CARGO) run --locked -p claude-proxy-cli -- --dry-run --endpoint "$(ENDPOINT)"
test:
	$(CARGO) test --workspace --locked
fmt:
	$(CARGO) fmt --all
lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	$(CARGO) test --workspace --locked
docker-up:
	docker compose up -d --build
docker-down:
	docker compose down
