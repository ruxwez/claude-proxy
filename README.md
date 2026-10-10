# claude-proxy — workspace Rust

Dos programas independientes, sin dependencia de Go:

```text
Cargo.toml                 workspace
crates/proxy/              claude-proxy: API Anthropic → proveedores
crates/cli/                claude-update-models: proxy → settings de Claude Code
Dockerfile                 imagen del proxy
docker-compose.yml         proxy con configuración desde .env
Makefile                   desarrollo, instalación y Docker
```

El proxy no modifica archivos de Claude Code. El CLI consulta el catálogo HTTP
del proxy y los límites de models.dev y configura Claude Code; no accede a las
claves de los proveedores.

Ambos crates son binarios. El CLI está en `crates/cli/src/main.rs`. En el proxy,
`main.rs` carga `.env` y arranca el servidor; `server.rs` atiende HTTP;
`provider.rs` consulta los proveedores; `translate.rs` convierte los mensajes y
`stream.rs` procesa SSE. No hay registro de conectores ni traits propios de
proveedor. Los proveedores comparten un cliente HTTP y la conversión de sesiones
es estable durante el proceso, sin caché ni bloqueos. Los tests están en
`src/tests.rs` dentro de cada crate.

## Arranque

Requisitos para desarrollo: Rust 1.99, Cargo y, opcionalmente, Make.
Para ejecutar en Docker: Docker Engine/Desktop y Compose.

```sh
make setup                 # crea .env solo si no existe
# Edita .env y añade OPENCODE_API_KEY.
make build                 # compila los dos binarios en target/release
make run                   # proxy en el puerto 10800
```

Sin Make:

```sh
cargo build --workspace --release --locked
cargo run --locked -p claude-proxy -- --env .env --port 10800
```

Las variables de entorno tienen prioridad sobre `.env`; los flags `--host` y
`--port` tienen prioridad sobre ambas fuentes. Un `.env` ausente permite usar solo
variables de entorno; un archivo existente pero inválido produce un error.
La clave de proveedor nunca se imprime en los logs.

## Docker y .env

```sh
make docker-up             # docker compose up -d --build
make docker-logs
make docker-down
```

Compose lee el archivo `.env` mediante `env_file` y lo inyecta al contenedor.
Dentro del contenedor el proxy escucha en `0.0.0.0`; en el host se publica en
`127.0.0.1:10800` con el `.env` predeterminado. El archivo real `.env` se conserva
durante la migración y se excluye de la imagen y de Git. El contenedor ejecuta
solo el proxy, como usuario sin privilegios, con healthcheck y apagado por SIGTERM.

También puedes usar Docker sin Compose:

```sh
docker build -t claude-proxy:rust .
docker run --rm --env-file .env -e PROXY_HOST=0.0.0.0 -p 127.0.0.1:10800:10800 claude-proxy:rust
```

El ejemplo presupone `PROXY_PORT=10800`. Si lo cambias en `.env`, ajusta el
mapeo de puertos con `docker run`; Compose lo calcula automáticamente.
No puede haber dos procesos escuchando en el mismo puerto: detén tu proxy
anterior antes de iniciar el nuevo en 10800.

## CLI: actualizar Claude Code

Instala el ejecutable nativo en el bin de Cargo (debe estar en PATH):

```sh
make install-cli
# Equivalente: cargo install --locked --path crates/cli
claude-update-models
```

El comando consulta `http://localhost:10800/v1/models`, actualiza el selector
`/model`, configura `ANTHROPIC_BASE_URL` y una clave ficticia de cliente, y
desactiva las betas experimentales que algunos proveedores rechazan.
Reinicia Claude Code para cargar los cambios.

En cada ejecución descarga `https://models.dev/catalog.json?type=all` y solo
sincroniza modelos con `limit.context >= 1000000`. Busca el registro exacto en
`providers[proveedor].models[id]`, incluidas variantes como `-contributor`.
Si falta un límite, solo usa el modelo de `models` identificado explícitamente
por `canonical_model_id`. Los límites del proveedor tienen prioridad; no se
infieren equivalencias con otros proveedores por similitud de nombre.
Si se publica `limit.input`, también debe ser al menos 990000: por ejemplo,
GPT-6 Luna queda excluido con entrada máxima de 922000, aunque su contexto total
sea 1050000. Así no necesita una configuración distinta al cambiar de modelo.
Muestra los modelos excluidos y su motivo. Si falla la
consulta, el formato no es válido o ninguno cumple el filtro, conserva intacta
la configuración. `--default-model` también debe cumplir el filtro.

Configura `CLAUDE_CODE_MAX_CONTEXT_TOKENS=990000` y
`CLAUDE_CODE_AUTO_COMPACT_WINDOW=900000` en `settings.json`, y elimina de ese
archivo los flags que desactivan la compactación o fuerzan 200K. Sustituye el
modelo actual y los alias retirados por modelos admitidos. Las preferencias
ajenas a la sincronización se conservan, con copia de seguridad antes de escribir.

El límite de 990K se aplica a los IDs personalizados que Claude no reconoce.
Claude puede conservar su ventana nativa para modelos reconocidos como Haiku;
la compactación a 900K se configura para todos. Los datos de models.dev no
garantizan que el proveedor permita toda esa ventana. Las variables de la shell,
los ajustes del proyecto y las políticas administradas pueden prevalecer.
Consulta la [documentación de contexto de Claude Code](https://code.claude.com/docs/en/model-config#correct-the-window-for-a-gateway-or-custom-model-id).

Para pruebas o un espejo del catálogo se puede usar `--models-url URL`.
Esta petición no incluye la credencial del proxy. El proxy y su Docker no
necesitan cambios para este filtro.

```sh
claude-update-models --dry-run
claude-update-models --endpoint http://localhost:10800/v1/models
claude-update-models --endpoint http://localhost:10800 --default-model opencode-go/mimo-v2.6-pro
claude-update-models --settings ./test-settings.json --dry-run
claude-update-models --help
```

También acepta `CLAUDE_PROXY_URL`, `CLAUDE_PROXY_API_KEY`, `--api-key` y
`--timeout`. La credencial del CLI es la del proxy, nunca `OPENCODE_API_KEY`.
El proxy actual acepta cualquier clave de cliente. La URL puede ser la raíz,
terminar en `/v1`, o incluir `/v1/models`, incluso detrás de un prefijo de ruta.

El archivo predeterminado es `$CLAUDE_CONFIG_DIR/settings.json` si esa variable
existe; si no, `~/.claude/settings.json` (Windows: `%USERPROFILE%\.claude\settings.json`).
`--settings` permite indicar otro archivo. Si es un enlace simbólico, indica
su destino explícitamente.

Comportamiento de sincronización:

- Obtiene todas las páginas, elimina duplicados y ordena por ID.
- Solicita `refresh=true` para evitar esperar los cinco minutos de caché.
- Reemplaza la lista del selector por el catálogo completo actual, retirando
  las entradas que ya no existen. Conserva los demás ajustes, permisos y plugins.
- Conserva el modelo predeterminado si sigue disponible, resolviendo los alias
  existentes. Si desaparece, elige DeepSeek v4.1 Flash cuando existe; si no,
  el primer modelo del catálogo. `--default-model` permite elegirlo explícitamente.
- Conserva el alias Haiku si sigue disponible; si no, usa el Haiku de versión
  más alta del catálogo. Si no hay ninguno, usa el modelo predeterminado.
  Repara también alias de modelo ya configurados que apunten a IDs retirados.
- Guarda una copia exacta en `backups/settings.before-model-sync.<uuid>.json`
  antes de cada cambio, bloquea actualizaciones simultáneas y reemplaza el archivo
  mediante un temporal en el mismo directorio. Una ejecución sin cambios no crea
  otra copia. Para restaurar, copia el backup sobre `settings.json` con Claude cerrado.
- Catálogos vacíos, parciales, JSON inválido, errores HTTP y paginación repetida
  cancelan la actualización. Un `settings.json` inválido tampoco se sobrescribe.
- `--dry-run` consulta y valida todo, pero no escribe archivos ni imprime claves.

El CLI sincroniza el catálogo anunciado: no consume cuota probando cada modelo.
El comando es manual; ejecútalo cuando quieras incorporar cambios de la API.
Las restricciones de modelos administradas por tu organización y las variables
exportadas por tu terminal pueden prevalecer sobre los ajustes de usuario.

## Conectores y protocolos

| Conector | Activación | Catálogo |
|---|---|---|
| `opencode-go` | `OPENCODE_GO_ENABLED=true` | Todo OpenCode Go |
| `opencode-zen` | `OPENCODE_ZEN_ENABLED=true` | Solo IDs que contienen `free` |
| `openai` | `OPENAI_ENABLED=true` | API compatible OpenAI |

Zen y Go comparten `OPENCODE_API_KEY`. El conector OpenAI usa `OPENAI_API_KEY`,
`OPENAI_BASE_URL` y, opcionalmente, `OPENAI_MODELS` (CSV para proveedores sin
catálogo). Las URLs de OpenCode también se pueden ajustar con
`OPENCODE_GO_BASE_URL` y `OPENCODE_ZEN_BASE_URL`.

Los IDs públicos son `conector/modelo`. Se conserva el enrutamiento del proyecto
Go original para OpenCode:

| Protocolo | Modelos |
|---|---|
| Anthropic `/messages` nativo | `claude-haiku-5-5`, `minimax-m*`, `qwen3.7-plus`, `qwen3.8-max`, `qwen3.8-flash` |
| OpenAI `/responses` | `muse-spark-*`, `grok-4.6*`, `grok-4.7*`, `gpt-6-luna*`, `gpt-5.6-luna*` |
| OpenAI `/chat/completions` | Resto de modelos |

El conector OpenAI genérico usa Chat Completions por defecto. Si un modelo
nuevo usa otro protocolo, configura el mapa JSON en `.env` sin recompilar:

```dotenv
MODEL_PROTOCOL_OVERRIDES='{"opencode-go/nuevo-modelo":"responses","openai/otro-modelo":"messages"}'
```

Las claves completas tienen prioridad sobre IDs sin prefijo. Valores permitidos:
`chat`, `messages`, `responses`. Reinicia el proxy tras editar `.env`.
Los catálogos se descubren dinámicamente, pero el protocolo de un modelo nuevo
puede necesitar este ajuste si no coincide con las reglas anteriores.

Se conservan los encabezados de cliente de OpenCode, la conversión estable de
sesiones y los IDs de petición del proyecto original. Las credenciales entrantes
se ignoran; la autenticación al proveedor siempre procede de `.env`.

## API

| Método | Ruta | Comportamiento |
|---|---|---|
| GET | `/health` | Estado, implementación Rust y conectores activos |
| GET | `/v1/models` | Modelos Anthropic con `has_more`, `first_id`, `last_id` |
| GET | `/v1/models/{conector}/{modelo}` | Un modelo concreto |
| POST | `/v1/messages` | Mensajes Anthropic, streaming o JSON |
| POST | `/v1/messages/count_tokens` | Estimación local aproximada de tokens |

El catálogo acepta `limit`, `after_id`, `before_id` y `refresh=true`. La caché
dura cinco minutos por conector. Un fallo parcial incluye `errors`, para que
el CLI pueda rechazar una lista incompleta; si fallan todos, devuelve 502.
La estimación de tokens usa caracteres/4 y no es un tokenizer del proveedor.

La traducción soporta texto, imágenes URL/base64, herramientas, resultados de
herramientas, `tool_choice`, mensajes de sistema intermedios y uso de tokens.
El streaming procesa SSE incrementalmente, conserva Unicode fragmentado y no
duplica los argumentos repetidos por Responses en eventos `.done`.
Un stream truncado o fallido produce un evento Anthropic `error`.
`REQUEST_TIMEOUT_SECONDS` limita la inactividad de lectura del proveedor, no
la duración total de la generación. La conexión tiene un límite de 15 segundos.
Durante el streaming se envía un evento `ping` cada 15 segundos para mantener
la conexión con Claude Code; esos pings no reinician el timeout del proveedor.
Los logs indican modelo, protocolo, estado HTTP, tiempo hasta las cabeceras y
errores de transporte/streaming, sin registrar prompts ni claves. En Docker:
`docker compose logs -f claude-proxy`.
Los campos específicos de Anthropic se conservan en el protocolo nativo;
en las conversiones solo se transmiten los campos con equivalente implementado.
No se traducen documentos/PDF ni bloques especiales de herramientas del servidor.
Responses no tiene equivalente a `stop_sequences` y `top_k`; se omiten.

## Verificación

```sh
make check                 # formato, Clippy sin warnings y tests
make test
```

Los tests cubren los tres protocolos contra un proveedor HTTP simulado,
autenticación, streaming, herramientas, sesiones, paginación, caché, errores,
validación y sincronización segura del archivo de Claude Code. No necesitan
claves reales ni consumen cuota.
