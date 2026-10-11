# Claude Proxy

Proxy Anthropic en Rust, con configuración desde una UI web y persistencia SQLite.
Arranca sin preguntas, claves, `.env` ni proveedores preconfigurados.

## Arranque

```sh
npm --prefix frontend ci
npm --prefix frontend run build
cargo run --locked -p claude-proxy
```

- UI + API interna: http://127.0.0.1:10801 (`/` y `/api/*`).
- API Anthropic: http://127.0.0.1:10800 (`/v1/*` y `/health`).
- Base de datos: `claude-proxy.sqlite3`, creada automáticamente.

La UI está hecha con Svelte 5, Vite, Tailwind CSS y bits-ui. `/providers` muestra
las conexiones; `/providers/{id}` contiene una única tabla por proveedor, con
búsqueda, filtros, activación y acciones por modelo. `/settings` reúne las
preferencias. Los formularios, metadatos y confirmaciones usan diálogos.

Activar un modelo lo guarda automáticamente en SQLite y mantiene su fila en la
misma tabla. Los IDs manuales y modelos guardados que ya no devuelve la API
siguen en esa tabla. Puedes editar nombre y protocolo desde el menú de la fila.
Los cambios se aplican sin reiniciar el proxy.

Los únicos flags de arranque son infraestructura opcional:

```sh
claude-proxy --database ./data/proxy.sqlite3 --host 127.0.0.1 --port 10800 --ui-port 10801
```

Los puertos deben ser distintos. Por defecto ambos servidores escuchan solo en
loopback. La administración local no pide contraseña y rechaza peticiones web
de otros orígenes y hosts DNS ajenos. Si usas `--host 0.0.0.0`, restringe el acceso
a la administración a tu red de confianza. La API Anthropic admite la credencial
ficticia del CLI. Las claves y tokens están en SQLite, sin cifrado de aplicación;
protege el archivo y sus copias. En Unix la base se crea con permisos 0600.

La configuración antigua de `.env` ya no se carga: configura los proveedores
desde la UI. El archivo existente se conserva.

## Proveedores

| Tipo | Credencial | Endpoint predeterminado |
|---|---|---|
| OpenCode Go | API key | `https://opencode.ai/zen/go/v1` |
| OpenAI API | API key | `https://api.openai.com/v1` |
| ChatGPT | Suscripción | `https://api.openai.com/v1` |
| Google Gemini | API key de Google AI Studio | `https://generativelanguage.googleapis.com/v1beta/openai` |
| API compatible OpenAI | API key opcional | URL propia, por ejemplo `http://localhost:8000/v1` |

Puedes crear varias instancias de cada tipo. Los IDs públicos son
`identificador-proveedor/id-modelo`; el ID del modelo puede contener `/`.
Solo se anuncian y aceptan los modelos configurados y habilitados. No hay filtros
por contexto. La UI consulta los modelos que devuelve el proveedor y los coteja
con models.dev; el catálogo publicado y el CLI solo usan tu selección guardada.

### Catálogo y cotejo con models.dev

La UI consulta `/models` con la credencial del proveedor (API key o suscripción).
Muestra todas las páginas del proveedor, deduplica IDs y permite buscar y marcar
los modelos que quieres activar. La conexión se edita en un modal. La clave se resuelve
en el servidor sin devolverla al navegador.

La API del proveedor solo determina los IDs disponibles. Todos los nombres,
límites de contexto, entrada/salida, precios por millón de tokens, razonamiento,
herramientas y modalidades proceden exclusivamente de
[models.dev](https://github.com/anomalyco/models.dev#api). Los campos ausentes se
muestran como `—`; puedes expandir los metadatos completos. No se utilizan los
límites ni capacidades que devuelva la API del proveedor.

**Añadir proveedor** permite crear OpenCode Go, OpenAI API, ChatGPT, Google o
cualquier API compatible. Cada proveedor tiene su ruta y consulta su catálogo
al abrirla. El botón de actualizar vuelve a consultarlo. Los errores de una
conexión no bloquean las demás ni ocultan los modelos guardados.

El cotejo usa el ID exacto dentro del proveedor de models.dev. Un
`canonical_model_id` de models.dev o un ID canónico exacto puede completar metadatos ausentes; los datos
específicos del proveedor prevalecen. No se inventan equivalencias por similitud
de nombre ni se excluyen los modelos sin coincidencia. Para APIs propias,
selecciona en el formulario el identificador de proveedor de models.dev con el
que quieres cotejar; ese identificador se guarda en SQLite.

models.dev se consulta desde el backend, sin credenciales del proveedor, con una
caché de 15 minutos. El botón de actualizar fuerza su actualización. Si falla,
se mantiene visible la lista obtenida de la API con un aviso. Si el proveedor no
ofrece catálogo, puedes seguir añadiendo IDs manualmente. La consulta no hace
peticiones de generación ni cambia tu selección guardada.

Cada modelo elige `chat` (Chat Completions), `responses` o `messages` (Anthropic).
Google utiliza su [compatibilidad OpenAI](https://ai.google.dev/gemini-api/docs/openai),
con Chat Completions; conserva `extra_content` de las herramientas, incluidas sus
firmas, tanto en JSON como SSE y en el siguiente turno del mismo modelo.
OpenCode mantiene sus encabezados de cliente y sesiones estables incluso con un
identificador personalizado de proveedor. Elige el protocolo que soporte cada
modelo; los modelos OpenAI con herramientas y razonamiento pueden requerir Responses.

### ChatGPT por suscripción

Añade un proveedor **ChatGPT** y pulsa **Conectar ChatGPT** en su página. La UI inicia
el [flujo oficial de registro e inicio de sesión para aplicaciones locales](https://developers.openai.com/siwc/token-sharing-open-source/sign-in):
PKCE, state de un solo uso, nonce y verificación del ID token con JWKS.
El callback usa `http://127.0.0.1:10801/auth/callback` (con tu puerto de UI).
Abre la UI desde el equipo que ejecuta el proxy para completar este callback local.

Se guardan los tokens en SQLite y se renuevan antes de las peticiones que lo
necesiten. La API interna nunca devuelve claves ni tokens. Desconectar intenta
revocar el acceso remoto y elimina los tokens locales; la UI informa si no se
pudo confirmar la revocación.

OpenAI API y ChatGPT son proveedores independientes. ChatGPT utiliza siempre
Responses y su sesión; OpenAI API utiliza siempre la API key. La UI no ofrece
una elección de credencial por modelo. La cuenta debe autorizar el uso de su
plan; un catálogo configurado no garantiza acceso a cada modelo.

Al arrancar, las configuraciones antiguas mixtas se migran en SQLite: se separan
los modelos de suscripción a un proveedor ChatGPT, se conservan claves y tokens,
y se actualiza el predeterminado si procede. Las conexiones que solo usaban
suscripción conservan su ID público. En conexiones mixtas, vuelve a ejecutar el
CLI para recibir los nuevos IDs del proveedor ChatGPT.

## CLI

```sh
cargo install --locked --path crates/cli
claude-update-models
# UI/API interna en otro equipo o puerto:
claude-update-models --endpoint http://127.0.0.1:10801
claude-update-models --dry-run
```

El CLI consulta `/api/cli-config`, obtiene modelos, modelo predeterminado, URL de
Anthropic y límites opcionales; solo inyecta esa configuración en Claude Code.
La URL Anthropic se calcula con el host de la API interna y el puerto real del
servidor Anthropic. Puedes sobrescribirla desde la UI si usas un reverse proxy.

No descarga metadatos externos ni exige una ventana de 990K. Los campos de
contexto y compactación mantienen los valores originales de 990000/900000.
El botón **Restaurar** de preferencias los repone; vacíos usan los valores de Claude.
Si configuras límites, son globales: deben ser compatibles con todos tus modelos.

El CLI conserva permisos, plugins y preferencias ajenas; repara alias retirados,
configura el selector y `ANTHROPIC_BASE_URL`, y mantiene el modelo actual si está
disponible y no hay un predeterminado indicado en la UI. `--default-model` permite
sobrescribirlo para una ejecución. Mantiene copias exactas en `backups/`, bloqueo,
escritura atómica, validación de catálogos y `--dry-run`. Un catálogo vacío o
inválido no modifica `settings.json`. Reinicia Claude Code después de sincronizar.

También admite `--settings`, `--timeout`, `--api-key`, `CLAUDE_PROXY_URL` y
`CLAUDE_PROXY_API_KEY`. El archivo predeterminado es
`$CLAUDE_CONFIG_DIR/settings.json` o `~/.claude/settings.json`.

## Docker

```sh
docker compose up -d --build
```

Publica ambos puertos en `127.0.0.1`, sin `.env`. El volumen `proxy-data` conserva
SQLite entre reinicios y recreaciones. El contenedor corre sin privilegios.
Para abrir la UI usa http://127.0.0.1:10801. OAuth requiere usar el navegador en el
host de Docker y mantener el mismo mapeo del puerto de UI para el callback local.

## Estructura

```text
frontend/src/              Svelte: rutas, tabla, preferencias y diálogos bits-ui
crates/proxy/src/
  main.rs                 composición y arranque de los dos listeners
  domain.rs               configuración, modelos, reglas y puerto SettingsStore
  application.rs          casos de uso, validación, snapshots y actualizaciones
  adapters/
    sqlite.rs             persistencia del documento de configuración en SQLite
    management.rs         UI + API interna, guardas de origen, adaptador del gateway
    oauth.rs              autorización OpenAI, validación y renovación
    anthropic.rs          API Anthropic, catálogo configurado y enrutamiento
    upstream.rs           llamadas HTTP autenticadas a los proveedores
    translate.rs          traducción de mensajes y estado entre protocolos
    stream.rs             traducción SSE incremental
  web/dist/               UI compilada de Svelte, incluida en el binario
crates/cli/src/main.rs     consulta la API interna e inyecta settings de Claude
```

Hexagonal sencilla: el dominio define una sola interfaz de persistencia,
la aplicación opera sobre ella y los adaptadores conectan SQLite y HTTP.
No hay framework frontend, build de Node, registro de conectores ni jerarquías de
traits para cada proveedor. SQLite guarda un único documento transaccional;
la configuración pequeña no necesita joins ni un ORM. Los snapshots en memoria
se renuevan después de persistir con éxito.

## API

| Puerto | Método y ruta | Uso |
|---|---|---|
| UI | `GET /api/settings` | Configuración con credenciales ocultas |
| UI | `PUT /api/settings` | Sustituir configuración; `api_key: null` conserva la clave, `""` la borra |
| UI | `POST /api/providers/catalog` | Consultar proveedor y cotejar modelos; recibe `provider`, `refresh`; la credencial se deriva del proveedor |
| UI | `GET /api/metadata/providers` | Proveedores disponibles en models.dev |
| UI | `GET /api/cli-config` | Configuración pública para el CLI |
| UI | `POST /api/providers/{id}/oauth` | Iniciar OAuth; devuelve URL de autorización |
| UI | `DELETE /api/providers/{id}/oauth` | Desconectar suscripción |
| UI | `DELETE /api/providers/{id}/api-key` | Borrar clave |
| Anthropic | `GET /health` | Estado del proceso |
| Anthropic | `GET /v1/models` | Catálogo habilitado, con paginación |
| Anthropic | `GET /v1/models/{proveedor}/{modelo}` | Modelo configurado |
| Anthropic | `POST /v1/messages` | JSON o SSE Anthropic |
| Anthropic | `POST /v1/messages/count_tokens` | Estimación local por caracteres/4 |

Las mutaciones de administración requieren `Content-Type: application/json`.
Los tokens OAuth recibidos del navegador se ignoran: solo el adaptador OAuth
puede actualizar las credenciales de suscripción. El `host_id` lo genera y
conserva el servidor. No habilites varios procesos contra la misma base: cada
proceso mantiene un snapshot y la renovación OAuth se serializa dentro de él.

## Traducción y streaming

La traducción conserva texto, imágenes, PDFs, herramientas, resultados y estado
de razonamiento. Responses acepta resultados multimedia directamente; Chat
recibe sus imágenes y archivos en un mensaje de usuario asociado, después de
todos los resultados paralelos. Los errores de herramientas conservan una marca
explícita. Se traducen JSON Schema, herramientas estrictas, rechazos, anotaciones
de fuentes y uso de caché. Messages mantiene el cuerpo, las betas y los errores
nativos. La [matriz de compatibilidad](docs/protocol-compatibility.md) detalla
las adaptaciones y las funciones sin equivalente.

Los streams SSE son incrementales, con pings cada 15 segundos, timeout de
inactividad y errores explícitos para streams truncados. No se registran prompts
ni claves. Las adaptaciones inevitables se identifican mediante
`x-claude-proxy-warnings` y códigos en el log; las funciones semánticas no
compatibles se rechazan antes de enviar la petición al proveedor.

### Razonamiento y flujos de herramientas

La traducción de razonamiento se limita a IDs OpenAI reconocidos y sus snapshots
con sufijo `-YYYY-MM-DD`. No presupone capacidades para alias personalizados,
variantes `-pro`, `-chat-latest` ni modelos desconocidos. El protocolo se elige explícitamente para cada modelo desde la UI.

| IDs reconocidos | Esfuerzo máximo traducido | Permiten `thinking: disabled` → `none` |
|---|---|---|
| `o1`, `o3`, `o3-mini`, `o4-mini`, `gpt-5`, `gpt-5-mini`, `gpt-5-nano` | `high` | No |
| `gpt-5.1` | `high` | Sí |
| `gpt-5.2`, `gpt-5.4`, `gpt-5.4-mini`, `gpt-5.4-nano`, `gpt-5.5` | `xhigh` | Sí |
| `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-6-sol`, `gpt-6-luna` | `max` | Sí |
| `gpt-6-astra`, `gpt-6.1-sol` | `max` | No |

`output_config.effort` (o `effort` como alternativa) se convierte en
`reasoning_effort` para Chat y `reasoning.effort` para Responses. `low`, `medium`
y `high` se conservan; `xhigh` y `max` se limitan al máximo de la tabla.
Con `thinking.type` igual a `enabled` o `adaptive` y sin esfuerzo reconocido se
usa `medium`. `budget_tokens` se omite: no existe una conversión exacta de ese
presupuesto a un nivel de esfuerzo. `disabled` tiene prioridad sobre el esfuerzo;
si el modelo no permite `none`, se omite el control y conserva su razonamiento
predeterminado. Los modelos desconocidos omiten todos estos controles.

Para los IDs reconocidos, Chat usa `max_completion_tokens` y Responses usa
`max_output_tokens`; ambos incluyen razonamiento y respuesta. Se omiten
`stop_sequences` y, salvo con esfuerzo explícito `none`, `temperature` y `top_p`.
Las secuencias de parada omitidas por el proveedor se aplican localmente,
también cuando se fragmentan entre eventos SSE. El consumo refleja los tokens
generados por el proveedor, incluidos los que se oculten después de la parada.
Estas reglas siguen la [referencia de Chat Completions](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)
y las [restricciones de parámetros de OpenAI](https://developers.openai.com/api/docs/guides/deployment-checklist).

GPT-5.4 y posteriores requieren Responses para herramientas con razonamiento;
GPT-6 Astra y GPT-6.1 Sol requieren Responses para cualquier llamada a herramientas.
Si se solicita una combinación incompatible en Chat, el proxy devuelve un error
que indica seleccionar Responses en la UI. No cambia automáticamente de
protocolo ni desactiva el razonamiento solicitado. Consulta la
[migración a Responses](https://developers.openai.com/api/docs/guides/migrate-to-responses)
y la [guía de razonamiento](https://developers.openai.com/api/docs/guides/reasoning).

Messages conserva `thinking`, `redacted_thinking`, sus firmas y los controles
nativos, también en streaming. Al convertir a Chat/Responses se omite el
pensamiento nativo de Anthropic: sus firmas no son intercambiables con el estado
de OpenAI. No se crean firmas ficticias ni se convierte razonamiento en texto público.
Véanse [pensamiento de Anthropic](https://platform.claude.com/docs/en/build-with-claude/thinking)
y [eventos SSE de Anthropic](https://platform.claude.com/docs/en/build-with-claude/streaming).

Responses solicita
`include: ["reasoning.encrypted_content"]` con `store: false`. Los items completos
de razonamiento se conservan en bloques `redacted_thinking` con el prefijo
`claude-proxy-state-v1:` y se restituyen en su posición original en el siguiente
turno. Los eventos parciales y los resúmenes no se convierten en respuesta pública.

DeepSeek `deepseek-flash` y `deepseek-v4-*`/`deepseek-v4.*` en Chat traducen
`enabled`/`adaptive` a pensamiento activado y `disabled` a desactivado. Su esfuerzo
se traduce a `low`, `high` o `max`; se omite `temperature` mientras piensa y se
rechazan herramientas forzadas incompatibles. Se conserva `reasoning_content`
de cada respuesta, incluidas las que no usan herramientas, y se devuelve en todos
los mensajes de asistente del historial. En SSE se reúnen sus fragmentos antes
de emitir el bloque de estado, seguido del texto o las herramientas.

Estos bloques contienen metadatos propios del proxy codificados en base64; no
son firmas ni ciphertext de Anthropic. Base64 no cifra el pensamiento de DeepSeek;
el contenido cifrado de Responses permanece tal como lo entrega el proveedor.
El estado pertenece al historial del cliente y está vinculado al identificador
completo `proveedor/modelo`, sin caché compartida entre conversaciones. Al cambiar
de modelo o protocolo, el estado incompatible se omite; nunca se envía como
redacción nativa a Messages. Si el cliente elimina estos bloques, el proxy no
puede reconstruir el estado perdido. Documentación:
[estado de razonamiento de Responses](https://developers.openai.com/cookbook/examples/responses_api/reasoning_items)
y [pensamiento con herramientas de DeepSeek](https://api-docs.deepseek.com/guides/thinking_mode/).

Las pruebas cubren llamadas paralelas, resultados y turnos sucesivos en JSON y
SSE. Además se comprobaron peticiones reales con GPT-6 Luna y DeepSeek V4.1 Flash,
y subagentes y Dynamic Workflows reales desde Claude Code. Consulta los modelos,
versiones, resultados y límites en [la verificación](docs/reasoning-verification.md).

## Verificación

```sh
make check
# o:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Las pruebas usan proveedores simulados, sin claves reales ni cuota. Cubren
SQLite, persistencia y actualización en caliente, ocultación de secretos,
separación de rutas, guardas de origen, estado OAuth y PKCE, firmas Google,
los tres protocolos, razonamiento, streaming y sincronización segura del CLI.
El login y la cuota reales dependen de la cuenta y se comprueban desde la UI.

Los resultados históricos de razonamiento están en
[docs/reasoning-verification.md](docs/reasoning-verification.md).
El script `scripts/verify_reasoning.py` permite repetir las comprobaciones
usando una copia aislada de SQLite; estas pruebas consumen cuota del proveedor.

## Desarrollo de la UI

```sh
npm --prefix frontend ci
npm --prefix frontend run dev
```

Vite sirve la UI en `http://127.0.0.1:5173` y reenvía `/api` y `/auth` al proxy
en 10801. Para compilar el binario local, ejecuta primero `npm --prefix frontend
run build` (también lo hace `make build` / `make run`). Docker compila la UI en
una etapa Node y la incluye en la etapa Rust; no requiere Node en ejecución.

Validación: `npm --prefix frontend run check`, `npm --prefix frontend test`,
`npm --prefix frontend run build` y `cargo test --workspace --locked`.
