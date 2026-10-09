# claude-proxy — atajos para compilar, testear y levantar el servidor.
#
# Uso:
#   make setup      # crea .env desde .env.example (si no existe)
#   make run        # levanta el servidor (lee .env)
#   make run PORT=9090 ENV=.env
#   make test vet check build clean
#
# Variables (override: make run PORT=9090):
#   ENV     fichero .env con las keys y flags (defecto: .env; "" = omitir)
#   PORT    puerto (defecto: 8080; "" = usar PROXY_PORT del .env)

SHELL := powershell.exe
.SHELLFLAGS := -NoProfile -Command

GO ?= go
BIN ?= claude-proxy
ENV ?= .env
PORT ?= 8080

.PHONY: help setup tidy vet test check build run clean

help: ## Muestra esta ayuda
	@echo "Targets disponibles:"
	@echo "  setup  - crea .env desde .env.example si no existe"
	@echo "  run    - levanta el servidor con ENV/PORT actuales"
	@echo "  test   - ejecuta los tests"
	@echo "  vet    - analisis estatico (go vet)"
	@echo "  check  - vet + test"
	@echo "  build  - compila el binario $(BIN).exe"
	@echo "  tidy   - ordena go.mod/go.sum"
	@echo "  clean  - borra el binario compilado"

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

run: ## Levanta el servidor (ENV y PORT configurables)
	$(GO) run ./cmd/server --env "$(ENV)" --port $(PORT)

clean: ## Borra el binario compilado
	Remove-Item -Force $(BIN).exe -ErrorAction SilentlyContinue
