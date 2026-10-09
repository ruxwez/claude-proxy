# claude-proxy — API Claude Like en Go

Recibe peticiones bajo el estándar **Anthropic Messages API** (`POST /v1/messages`)
y las redirige al proveedor real según el **modelo** indicado, usando un fichero `.toml`.

Proveedor de referencia implementado: **OpenAI** (`/chat/completions`).
Modelos gratuitos de **OpenCode Zen**: `provider = "opencode"` (`/chat/completions`)
y `provider = "opencode-responses"` (`/responses`).
La arquitectura es por **adaptadores**: añadir Gemini, Ollama, Mistral, etc.
es implementar la interfaz `adapter.Adapter`.

## Endpoints expuestos (compat Anthropic)

| Método | Ruta                          | Descripción                                    |
|--------|-------------------------------|------------------------------------------------|
| POST   | `/v1/messages`                | Crear mensaje (stream y no-stream)             |
| POST   | `/v1/messages/count_tokens`   | Estimación local de tokens                    |
| GET    | `/v1/models`                  | Lista los `anthropic_model` configurados       |
| GET    | `/health`                     | `{"status":"ok"}`                              |

Headers de entrada aceptados: `x-api-key`, `anthropic-version`, `content-type`.
La `api-key` del cliente **no** se reenvía: se usa la del proveedor según la ruta.

## Configuración `.toml`

```toml
[server]
host = "0.0.0.0"
port = 8080
# auth_token = "sk-proxy-secreto"  # si se define, el cliente debe enviarlo
                                  # vía x-api-key o Authorization: Bearer

# Ruta por defecto cuando ningún [[routes]] coincide.
[default]
provider = "openai"
target_model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
timeout_seconds = 120

[[routes]]
anthropic_model = "claude-3-5-sonnet-20241022"
provider = "openai"
target_model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
timeout_seconds = 120

# Comodines estilo path.Match (solo `*`): el primero que coincida gana.
[[routes]]
anthropic_model = "claude-3-opus-*"
provider = "openai"
target_model = "gpt-4o"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

# Passthrough a Anthropic real (sin traducción).
[[routes]]
anthropic_model = "claude-3-haiku-*"
provider = "anthropic"
target_model = "claude-3-haiku-20240307"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"
```

Notas:

- `anthropic_model`: nombre que el cliente envía en `"model"`. Acepta `*`.
- `provider`: `openai` | `anthropic` | `opencode` | `opencode-responses` (extensible).
- `target_model`: modelo real del proveedor. `"*"` = reenviar el modelo
  solicitado tal cual (útil para flotas como los `*-free` de Zen).
- API key (orden): `api_key` (expande `$ENV` / `${ENV}`) → `api_key_env` → `""`
  (vacío permitido para proveedores locales tipo Ollama).
- `base_url` por defecto: OpenAI `https://api.openai.com/v1`,
  Anthropic `https://api.anthropic.com/v1`,
  opencode/opencode-responses `https://opencode.ai/zen/v1` (se recorta `/` final).
- `timeout_seconds` por defecto: `120`.

## Modelos gratuitos de OpenCode Zen

Verificado contra `GET https://opencode.ai/zen/v1/models` el 09/10/2026.
Son gratis por tiempo limitado ([precios Zen](https://opencode.ai/docs/zen/#pricing)).
Config lista para usar: **`config.opencode.toml`**.

```powershell
# 1. Pon tu key de https://opencode.ai/auth (Zen) en el .env:
Copy-Item .env.example .env   # y rellena OPENCODE_API_KEY
# 2. Arranca con la config de gratuitos:
go run ./cmd/server --config config.opencode.toml
# 3. Pide con el ID de Zen como "model":
curl http://localhost:8080/v1/messages `
  -H "content-type: application/json" `
  -d '{"model":"exo-free","max_tokens":256,
       "messages":[{"role":"user","content":"Hola, ¿quién eres?"}]}'
```

| ID Zen (= `"model"`) | provider en el toml | Endpoint Zen | Notas |
|---|---|---|---|
| `big-pickle` | `opencode` | `/chat/completions` | stealth, gratis temporal |
| `space-bunny-free` | `opencode` | `/chat/completions` | zero-retention |
| `longcat-2.5-preview-free` | `opencode` | `/chat/completions` | zero-retention |
| `step-5-preview-free` | `opencode` | `/chat/completions` | zero-retention |
| `exo-free` | `opencode` | `/chat/completions` | gratis temporal |
| `mimo-v2.6-flash-free` | `opencode` | `/chat/completions` | gratis temporal |
| `mimo-v2.5-free` | `opencode` | `/chat/completions` | en docs; ausente del `/models` en vivo el 09/10/2026 |
| `ling-3.1-flash-free` | `opencode` | `/chat/completions` | gratis temporal |
| `ling-3.0-flash-fin-free` | `opencode` | `/chat/completions` | gratis temporal |
| `nemotron-3-ultra-free` | `opencode` | `/chat/completions` | trial NVIDIA, no enviar datos sensibles |
| `nemotron-3.5-lightning-free` | `opencode` | `/chat/completions` | trial NVIDIA, no enviar datos sensibles |
| `muse-spark-1.3-contributor-free` | `opencode-responses` | `/responses` | Zen solo lo sirve en Responses API; entrena con tus prompts |
| `muse-spark-1.2-contributor-free` | `opencode-responses` | `/responses` | solo en `/models` en vivo, no en docs |

Notas:

- `config.opencode.toml` incluye además la ruta comodín `*-free` con
  `target_model = "*"`: futuros modelos gratis funcionan sin editar el fichero.
- Limitaciones de `opencode-responses`: sin `stop_sequences`/`top_k`
  (Responses API no los contempla; se ignoran).
- No incluido: `jev-1.13-free` (endpoint `/systemone`, no es un chat y no
  admite el estándar Anthropic Messages).
- Privacidad: durante el periodo gratuito varios proveedores pueden usar los
  datos para mejorar el modelo (ver [privacidad Zen](https://opencode.ai/docs/zen/#privacy)).
- OpenCode Go (suscripción) usa otra base: `https://opencode.ai/zen/go/v1`
  (ver bloque comentado al final de `config.opencode.toml`).

## Ejecutar

Requisitos: Go 1.23+ (comprueba con `go version`).

```powershell
# 1. Clona/entrar al proyecto y crea tu .env con las keys:
Copy-Item .env.example .env
notepad .env   # rellena OPENCODE_API_KEY (https://opencode.ai/auth) y/o OPENAI_API_KEY

# 2. Elige config: gratuita (Zen) o ejemplo (OpenAI):
go run ./cmd/server --config config.opencode.toml      # modelos *-free
# go run ./cmd/server --config config.example.toml      # OpenAI

# 3. Comprueba que responde:
curl http://localhost:8080/health
curl http://localhost:8080/v1/models
```

Flags: `--config` (defecto `config.toml`), `--env` (defecto `.env`;
fichero ausente = se usan las variables del sistema, `--env ""` lo omite),
`--port` y `--host` (override del toml).

Con `make` (hay `Makefile`):

```powershell
make setup      # crea .env desde .env.example si no existe
make run-free   # config.opencode.toml (modelos gratis Zen)
make run-openai # config.example.toml (OpenAI)
make run PORT=9090                    # override de puerto
make run CONFIG=config.toml ENV=.env  # personalizado
make check      # vet + test
make build      # genera claude-proxy.exe
```

Compilado:

```powershell
go build -o claude-proxy ./cmd/server
.\claude-proxy --config config.opencode.toml --port 8080
```

## Ejemplo de petición

```powershell
curl http://localhost:8080/v1/messages `
  -H "content-type: application/json" `
  -H "x-api-key: lo-que-sea" `
  -H "anthropic-version: 2023-06-01" `
  -d '{
    "model": "claude-3-5-sonnet-20241022",
    "max_tokens": 256,
    "system": "Responde en español.",
    "messages": [{"role": "user", "content": "Dime qué es un adaptador en 1 línea."}]
  }'
```

Streaming (`"stream": true`) devuelve SSE con eventos Anthropic
(`message_start`, `content_block_start`, `content_block_delta`, …)
traducidos desde el SSE de OpenAI.

## Traducción Anthropic ↔ OpenAI

- `system` (string o bloques) → mensajes `system` de OpenAI.
- Bloques `text` / `image` (base64 → `image_url` data-URI) / `tool_use` → `tool_calls`
  / `tool_result` → mensajes `tool`.
- `tools` (`input_schema`) → `tools[].function.parameters`.
- `tool_choice` (`auto`/`any`/`tool`/`none`) → equivalente OpenAI.
- `stop_sequences` → `stop`, `max_tokens` → `max_tokens`.
- Respuesta: `content` + `tool_calls` → bloques `text` + `tool_use`,
  `usage` (`prompt_tokens`/`completion_tokens` → `input_tokens`/`output_tokens`),
  `finish_reason` (`tool_calls`→`tool_use`, `length`→`max_tokens`, resto→`end_turn`).
- Streaming: deltas de texto y `partial_json` de herramientas + secuencia
  de eventos Anthropic válida (cierra bloques de texto antes de abrir `tool_use`).

## Añadir un adaptador nuevo

1. Implementa `adapter.Adapter` (`internal/adapter/adapter.go`):

```go
type Adapter interface {
    Provider() string
    Handle(ctx context.Context, anthReq *anthropic.MessagesRequest, route config.ResolvedRoute, w http.ResponseWriter, r *http.Request) error
}
```

2. Regístralo con `adapter.Register` (ver `init()` en `internal/adapter/openai`).
3. Úsalo en el `.toml` con `provider = "mi-proveedor"`.

Ver `internal/adapter/openai` como referencia completa (no-stream + stream).

## Estructura

```text
cmd/server/main.go
config.example.toml
config.opencode.toml             # rutas de los modelos gratuitos de OpenCode Zen
internal/anthropic/types.go      # tipos del estándar Anthropic
internal/openai/types.go         # tipos del estándar OpenAI Chat Completions
internal/responses/types.go      # tipos de OpenAI Responses API
internal/config/config.go        # carga .toml + resolución por modelo
internal/translate/translate.go  # Anthropic→OpenAI y OpenAI→Anthropic
internal/translate/stream.go     # SSE OpenAI → SSE Anthropic
internal/translate/responses.go  # Anthropic↔Responses (+responses_stream.go)
internal/adapter/adapter.go      # interfaz + registry
internal/adapter/openai/openai.go
internal/adapter/anthropic/anthropic.go
internal/adapter/opencode/opencode.go      # provider "opencode" (Zen chat)
internal/adapter/opencode/responses.go     # provider "opencode-responses"
internal/server/server.go        # mux, middlewares, handlers
```
