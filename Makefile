CARGO ?= cargo
DATABASE ?= claude-proxy.sqlite3
UI_PORT ?= 10801
PORT ?= 10800
ENDPOINT ?= http://localhost:10801

ifeq ($(OS),Windows_NT)
SHELL := powershell.exe
.SHELLFLAGS := -NoProfile -Command
else
SHELL := /bin/sh
endif

.PHONY: help setup ui build run update-models preview-models install-cli test fmt lint check clean docker-build docker-up docker-down docker-logs

help:
	@echo "setup build run test fmt lint check install-cli update-models preview-models"
	@echo "docker-build docker-up docker-down docker-logs clean"
	@echo "DATABASE=claude-proxy.sqlite3 PORT=10800 UI_PORT=10801 ENDPOINT=http://localhost:10801"

setup:
	@echo "No requiere configuración inicial. Ejecuta make run y abre http://127.0.0.1:$(UI_PORT)"

ui:
	npm --prefix frontend ci --no-fund --no-audit
	npm --prefix frontend run build

build: ui
	$(CARGO) build --workspace --release --locked

run: ui
	$(CARGO) run --locked -p claude-proxy -- --database "$(DATABASE)" --port $(PORT) --ui-port $(UI_PORT)

install-cli:
	$(CARGO) install --locked --path crates/cli

update-models:
	$(CARGO) run --locked -p claude-update-models -- --endpoint "$(ENDPOINT)"

preview-models:
	$(CARGO) run --locked -p claude-update-models -- --endpoint "$(ENDPOINT)" --dry-run

test: ui
	$(CARGO) test --workspace --locked

fmt:
	$(CARGO) fmt --all

lint: ui
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

check: ui
	npm --prefix frontend run check
	npm --prefix frontend test
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	$(CARGO) test --workspace --locked

clean:
	$(CARGO) clean

docker-build:
	docker compose build

docker-up:
	docker compose up -d --build

docker-down:
	docker compose down

docker-logs:
	docker compose logs -f
