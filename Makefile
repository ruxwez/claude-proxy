# claude-proxy — atajos para compilar, testear y levantar el servidor.
#
# Uso:
#   make setup          # crea .env desde .env.example (si no existe)
#   make run-free       # levanta con los modelos gratuitos de OpenCode Zen
#   make run-openai     # levanta con la config de ejemplo (OpenAI)
#   make run CONFIG=config.toml ENV=.env PORT=8080   # personalizado
#   make test vet build clean
#
# Variables (override: make run PORT=9090):
#   CONFIG  fichero .toml de rutas (defecto: config.opencode.toml)
#   ENV     fichero .env con las keys (defecto: .env; "" = omitir)
#   PORT    puerto (defecto: 8080; "" = usar el del .toml)

SHELL := powershell.exe
.SHELLFLAGS := -NoProfile -Command

GO ?= go
BIN ?= claude-proxy
CONFIG ?= config.opencode.toml
ENV ?= .env
PORT ?= 8080

.PHONY: help setup tidy vet test check build run run-free run-openai clean

help: ## Muestra esta ayuda
	@echo "Targets disponibles:"
	@echo "  setup      - crea .env desde .env.example si no existe"
	@echo "  run-free   - levanta con config.opencode.toml (gratis Zen)"
	@echo "  run-openai - levanta con config.example.toml (OpenAI)"
	@echo "  run        - levanta con CONFIG/ENV/PORT actuales"
	@echo "  test       - ejecuta los tests"
	@echo "  vet        - analisis estatico (go vet)"
	@echo "  check      - vet + test"
	@echo "  build      - compila el binario $(BIN).exe"
	@echo "  tidy       - ordena go.mod/go.sum"
	@echo "  clean      - borra el binario compilado"

setup: ## Crea .env desde .env.example si no existe
	@if (!(Test-Path .env)) { Copy-Item .env.example .env; echo ".env creado, editalo y rellena tus keys." } else { echo ".env ya existe, no se toca." }

tidy: ## Ordena dependencias
	$(GO) mod tidy

vet: ## Analisis estatico
	$(GO) vet ./...

test: ## Ejecuta los tests
	$(GO) test ./... -count=1

check: vet test ## vet + test

build: ## Compila el binario
	$(GO) build -o $(BIN).exe ./cmd/server

run: ## Levanta el servidor (CONFIG, ENV y PORT configurables)
	$(GO) run ./cmd/server --config $(CONFIG) --env "$(ENV)" --port $(PORT)

run-free: CONFIG = config.opencode.toml
run-free: run ## Levanta con los modelos gratuitos de OpenCode Zen

run-openai: CONFIG = config.example.toml
run-openai: run ## Levanta con la config de ejemplo (OpenAI)

clean: ## Borra el binario compilado
	Remove-Item -Force $(BIN).exe -ErrorAction SilentlyContinue
