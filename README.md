# claude-proxy — API estilo Claude con conectores

Servidor en Go que expone la **API de Anthropic Messages** (`POST /v1/messages`)
y **traduce cada petición al estándar del conector** indicado en el modelo.

En lugar de un fichero de rutas (TOML), los proveedores son **conectores**
que se activan con **variables booleanas del `.env`**. El modelo pedido lleva
el prefijo del conector: `conector/modelo`.

Conectores incluidos:

| Conector         | Base URL                          | Qué expone |
|------------------|-----------------------------------|------------|
| `opencode-zen`   | `https://opencode.ai/zen/v1`      | Solo modelos gratuitos (IDs con `free`) |
| `opencode-go`    | `https://opencode.ai/zen/go/v1`   | Todo el catálogo Go |
| `openai`         | configurable (`OPENAI_BASE_URL`)  | Modelos de cualquier API compatible OpenAI |

Zen y Go comparten la misma API key (`OPENCODE_API_KEY`).

> **Aviso sobre el tier gratuito de Zen**: OpenCode bloquea el tier gratuito
> fuera de la app OpenCode. Con la key real, los modelos `*-free` de Zen
> devuelven `403 "OpenCode's free tier can only be used from within OpenCode"`.
> El conector los lista, pero no sirven chat desde el proxy. Usa el conector
> Go (suscripción) para modelos que sí funcionan.

### Protocolos por modelo

Cada modelo se enruta al estándar que soporta (ver
[endpoints de Go](https://opencode.ai/docs/go/#endpoints)):

- `/messages` **nativo Anthropic**: `claude-haiku-5-5`, `minimax-m*`,
  `qwen3.7-plus`, `qwen3.8-max`, `qwen3.8-flash`. Se reenvían tal cual
  (sin traducción), lo que conserva `tool_use`, `cache_control`, etc.
- `/responses`: `grok-4.6`, `grok-4.7`, `gpt-6-luna`, `gpt-5.6-luna`,
  `muse-spark-*`. Se traducen desde/hacia Anthropic.
- `/chat/completions`: el resto (GLM, Kimi, DeepSeek, LongCat, MiMo, …).

## Endpoints

| Método | Ruta                          | Descripción |
|--------|-------------------------------|-------------|
| POST   | `/v1/messages`                | Crear mensaje (stream y no-stream), estilo Anthropic |
| POST   | `/v1/messages/count_tokens`   | Estimación local de tokens (chars/4) |
| GET    | `/v1/models`                  | Agrega los modelos de todos los conectores activos |
| GET    | `/v1/models/{conector}/{modelo}` | Un modelo concreto |
| GET    | `/health`                     | Estado y conectores activos |

Cabeceras de entrada aceptadas: `x-api-key`, `anthropic-version`,
`content-type`. La `x-api-key` del cliente **no** se reenvía: se usa la key
del proveedor configurada en el `.env`.

### Formato de `/v1/models` (estilo Anthropic)

```json
{
  "data": [
    {
      "type": "model",
      "id": "opencode-zen/exo-free",
      "display_name": "exo-free",
      "created_at": "2026-10-09T21:13:50Z"
    }
  ],
  "has_more": false,
  "first_id": "opencode-zen/exo-free",
  "last_id": "opencode-zen/step-5-preview-free"
}
```

Por defecto devuelve **todos** los modelos. Con `?limit=N` se recortan la
respuesta y `has_more` se pone a `true`.

## Modelo con prefijo

El campo `"model"` de `POST /v1/messages` se identifica como
`conector/modelo`, p. ej.:

- `opencode-zen/exo-free` → Zen (modelo gratuito).
- `opencode-go/minimax-m3` → Go (catálogo completo).
- `openai/gpt-4o-mini` → conector OpenAI genérico.

Los modelos gratuitos incluyen `free` en su propio ID (así lo sirve Zen),
por lo que siempre es visible si un modelo es gratuito.

## Configuración `.env`

```powershell
Copy-Item .env.example .env   # y rellena las keys
```

| Variable | Defecto | Descripción |
|---|---|---|
| `PROXY_HOST` | `0.0.0.0` | Interfaz de escucha |
| `PROXY_PORT` | `8080` | Puerto |
| `PROXY_AUTH_TOKEN` | (vacío) | Si se define, el cliente debe enviarlo por `x-api-key` o `Authorization: Bearer` |
| `REQUEST_TIMEOUT_SECONDS` | `120` | Timeout por llamada al proveedor |
| `OPENCODE_API_KEY` | (vacío) | API key de OpenCode (Zen y Go) — https://opencode.ai/auth |
| `OPENCODE_ZEN_ENABLED` | `false` | Activa el conector Zen (solo `free`) |
| `OPENCODE_GO_ENABLED` | `false` | Activa el conector Go (todo) |
| `OPENAI_ENABLED` | `false` | Activa el conector OpenAI genérico |
| `OPENAI_API_KEY` | (vacío) | Key del proveedor OpenAI |
| `OPENAI_BASE_URL` | `https://api.openai.com/v1` | Base de la API compatible OpenAI |
| `OPENAI_MODELS` | (vacío) | CSV opcional; si se define no se consulta `/models` |

Los booleanos aceptan `true/false`, `1/0`, `yes/no`, `on/off`, `si/no`.

## Ejecutar

Requisitos: Go 1.23+.

```powershell
# 1. Configura el .env
Copy-Item .env.example .env
notepad .env            # rellena OPENCODE_API_KEY y activa conectores

# 2. Arranca
go run ./cmd/server             # lee .env y usa PROXY_PORT
go run ./cmd/server --port 9090 # override del puerto
go run ./cmd/server --env ""    # usa solo el entorno del sistema
```

Con `make`:

```powershell
make setup   # crea .env desde .env.example
make run     # levanta el servidor
make check   # vet + test
make build   # genera claude-proxy.exe
```

Compilado:

```powershell
go build -o claude-proxy.exe ./cmd/server
.\claude-proxy.exe
```

## Ejemplos

Listar conectores y modelos:

```powershell
curl http://localhost:8080/health
curl http://localhost:8080/v1/models
```

Petición estilo Anthropic:

```powershell
curl http://localhost:8080/v1/messages `
  -H "content-type: application/json" `
  -H "x-api-key: lo-que-sea" `
  -H "anthropic-version: 2023-06-01" `
  -d '{
    "model": "opencode-go/minimax-m3",
    "max_tokens": 256,
    "system": "Responde en español.",
    "messages": [{"role": "user", "content": "¿Quién eres?"}]
  }'
```

Con el SDK de Anthropic, apunta `base_url` al proxy:

```python
from anthropic import Anthropic
client = Anthropic(base_url="http://localhost:8080", api_key="lo-que-sea")
client.messages.create(
    model="opencode-zen/exo-free",
    max_tokens=256,
    messages=[{"role": "user", "content": "Hola"}],
)
```

El streaming (`"stream": true`) devuelve SSE con eventos Anthropic
(`message_start`, `content_block_start`, `content_block_delta`, …).

## Usar con Claude Code

El proxy es compatible con Claude Code (probado con v2.1.296, chat + tool
calling). Apunta la base URL al proxy (raíz, **sin `/v1`):

```powershell
$env:ANTHROPIC_BASE_URL   = "http://localhost:8080"
$env:ANTHROPIC_AUTH_TOKEN = "dummy"        # o tu PROXY_AUTH_TOKEN
$env:ANTHROPIC_MODEL      = "opencode-go/minimax-m3"

# Slots internos (evita 404 en llamadas de fondo con el modelo "haiku")
$env:ANTHROPIC_DEFAULT_OPUS_MODEL   = "opencode-go/minimax-m3"
$env:ANTHROPIC_DEFAULT_SONNET_MODEL = "opencode-go/minimax-m3"
$env:ANTHROPIC_DEFAULT_HAIKU_MODEL  = "opencode-go/glm-5.3-flash"

$env:CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = "1"
$env:DISABLE_TELEMETRY = "1"

claude
```

Notas:

- **Sesión**: Go exige `x-opencode-session`. Claude Code envía su propia
  cabecera (`X-Claude-Code-Session-Id`) y el proxy la reenvía; si no llega
  ninguna, genera una.
- **Modelo personalizado**: `ANTHROPIC_MODEL`/`--model` aceptan el ID con
  prefijo directamente. Claude Code muestra un aviso informativo de que no
  conoce el modelo (asume 200k de contexto); se silencia con
  `CLAUDE_CODE_MAX_CONTEXT_TOKENS=<n>` o mapeándolo con `modelPicker`/
  `modelOverrides`. No impide funcionar.
- **`mid-conversation-system`**: el proxy soporta mensajes con
  `role: "system"` dentro de `messages` (los usa Claude Code).
- Elige modelos con buen soporte de *tool calling* para tareas de agente
  (MiniMax, GLM, Kimi, DeepSeek, Qwen, Claude Haiku…).

## Traducción

- **Messages nativo** (`/messages`): se reenvía la petición Anthropic tal
  cual (solo se cambia el `model`), autenticando con `x-api-key`. Camino
  preferido cuando el modelo lo soporta.
- **Chat Completions** (Zen, Go, OpenAI): `system` → mensaje `system`,
  bloques `text`/`image`/`tool_use`/`tool_result` ↔ `messages`/`tool_calls`,
  `tools` (`input_schema` → `parameters`), `tool_choice`, `stop_sequences`,
  `max_tokens`; respuesta y streaming traducidos a eventos Anthropic.
- **Responses API**: los modelos `grok`, `gpt-luna` y `muse-spark` se
  sirven por `/responses`; el conector los traduce automáticamente.
- Los mensajes `role: "system"` a mitad de conversación se soportan en las
  tres rutas.

## Estructura

```text
cmd/server/main.go                       # arranque + registro de conectores
internal/config/config.go                # Settings desde el entorno
internal/env/env.go                      # carga .env + helpers (bool/int/CSV)
internal/anthropic/types.go              # tipos del estándar Anthropic
internal/openai/types.go                 # tipos de Chat Completions
internal/responses/types.go              # tipos de la Responses API
internal/translate/                      # Anthropic↔OpenAI y Anthropic↔Responses
internal/connector/connector.go          # interfaz, registro, helpers HTTP
internal/connector/opencode/opencode.go  # conectores Zen y Go
internal/connector/openai/openai.go      # conector OpenAI genérico
internal/server/server.go                # mux, auth, /v1/models, /v1/messages
```

## Añadir un conector

1. Implementa `connector.Connector`:

```go
type Connector interface {
    Name() string
    Models(ctx context.Context) ([]connector.Model, error)
    Chat(ctx context.Context, req *anthropic.MessagesRequest, targetModel string, w http.ResponseWriter, r *http.Request) error
}
```

2. Regístralo en `buildRegistry` (`cmd/server/main.go`) tras leer su flag
   booleano del entorno.

Ver `internal/connector/openai` como referencia completa (listado + chat).
