CARGO ?= cargo
ENV ?= .env
PORT ?= 10800
ENDPOINT ?= http://localhost:10800

ifeq ($(OS),Windows_NT)
SHELL := powershell.exe
.SHELLFLAGS := -NoProfile -Command
else
SHELL := /bin/sh
endif

.PHONY: help setup build run update-models preview-models install-cli test fmt lint check clean docker-build docker-up docker-down docker-logs

help:
	@echo "setup build run test fmt lint check install-cli update-models preview-models"
	@echo "docker-build docker-up docker-down docker-logs clean"
	@echo "ENV=.env PORT=10800 ENDPOINT=http://localhost:10800"

setup:
ifeq ($(OS),Windows_NT)
	@if (!(Test-Path .env)) { Copy-Item .env.example .env }
else
	@test -f .env || cp .env.example .env
endif

build:
	$(CARGO) build --workspace --release --locked

run:
	$(CARGO) run --locked -p claude-proxy -- --env "$(ENV)" --port $(PORT)

install-cli:
	$(CARGO) install --locked --path crates/cli

update-models:
	$(CARGO) run --locked -p claude-update-models -- --endpoint "$(ENDPOINT)"

preview-models:
	$(CARGO) run --locked -p claude-update-models -- --endpoint "$(ENDPOINT)" --dry-run

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
