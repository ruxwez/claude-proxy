# Claude Proxy

Monorepo Rust con motor de conversión independiente, servidor hexagonal y panel
TUI con Ratatui. No requiere Node ni incluye una UI web.

## Arranque

```sh
cargo build --workspace --locked
cargo run --locked -p claude-proxy-server
# En otro terminal:
cargo run --locked -p cli
```

Para instalar los ejecutables:

```sh
cargo install --locked --path crates/proxy
cargo install --locked --path crates/cli
claude-proxy-server
claude-proxy
```

El ejecutable del panel es `claude-proxy`; su paquete es `cli`. El servidor es
`claude-proxy-server`. Por defecto escucha en loopback:

| Servicio | Endpoint |
|---|---|
| Administración REST | `http://127.0.0.1:10801/api/*` |
| API Anthropic | `http://127.0.0.1:10800/v1/*` |
| Salud del gateway | `http://127.0.0.1:10800/health` |

El CLI busca primero el servidor local en 10801. Si no está disponible, prueba
el endpoint guardado y finalmente solicita otro. Verifica la identidad mediante
`/api/status` antes de guardar la conexión. `--endpoint` o `CLAUDE_PROXY_URL`
seleccionan explícitamente un servidor. El puerto Anthropic real se obtiene de
la API; admite una URL pública propia en preferencias.

El endpoint se escribe atómicamente en la carpeta convencional de configuración
mediante `directories::ProjectDirs`:

- Windows: `%APPDATA%/claude-proxy/config/config.json`.
- Linux: `$XDG_CONFIG_HOME/claude-proxy/config.json` o `~/.config/claude-proxy/config.json`.
- macOS: `~/Library/Application Support/claude-proxy/config.json`.

Ese archivo contiene únicamente el endpoint REST. El servidor guarda su
infraestructura en `server/` dentro de esa carpeta; `--config-dir` permite
cambiarla. No se leen claves de `.env` en producción.

## Primera configuración y bases de datos

Si `/api/status` indica que falta configurar el servidor, el TUI abre un
asistente obligatorio para elegir `sqlite`, `postgres` o `mysql`.

Para SQLite, deja vacía la ruta para utilizar `server/proxy.sqlite3`. Las rutas
se interpretan en el equipo del servidor. Para PostgreSQL/MySQL introduce el DSN,
por ejemplo `postgres://usuario:clave@host/base` o `mysql://usuario:clave@host/base`.
La base remota debe existir y el usuario necesita permisos para crear la tabla
`settings`. Las opciones TLS se indican en el DSN según SQLx.

El servidor comprueba la conexión y persiste la selección en `server/database.json`.
Las credenciales de los proveedores y los tokens OAuth permanecen en la base de
datos elegida; el CLI recibe únicamente su estado. El DSN permanece en el servidor.
La selección inicial solo se acepta una vez. Para cambiar infraestructura después,
detén el servidor y modifica su archivo `database.json`; conserva una copia de la
base y migra los datos si seleccionas otra base vacía.

`--database /ruta/archivo.sqlite3` permite abrir directamente una base SQLite existente.
Si encuentra el antiguo `claude-proxy.sqlite3` en el directorio de arranque y no hay
configuración nueva, lo reutiliza y recuerda su ruta absoluta. No copia una base
SQLite activa ni pierde sus credenciales. También mantiene la migración de las
antiguas conexiones mixtas OpenAI/ChatGPT.

La configuración es un documento JSON transaccional, sin ORM ni tablas por cada
modelo. Las escrituras renuevan el snapshot del gateway después de persistir.
SQLite usa WAL; PostgreSQL/MySQL usan SQLx y un trabajador dedicado. Solo debe
haber un proceso de servidor por base, porque los snapshots son locales al proceso.

## Panel de terminal

El panel tiene un menú lateral: elige Proveedores, Modelos, Preferencias o Trazas
con `↑↓` y abre la sección con `Enter`. Dentro, selecciona un elemento con `↑↓`
y pulsa `Enter` para ver sus acciones. El menú incluye añadir, editar, activar,
consultar y sincronizar con Claude Code; también funciona cuando la lista está vacía.
`Esc` vuelve al nivel anterior y `F1` muestra la ayuda.

En formularios, `↑↓` o `Tab` cambian de campo. Los tipos de proveedor, protocolos,
bases de datos y opciones Sí/No se eligen con `←→`; no hay que escribir sus nombres.
`Enter` avanza al siguiente campo: solo guarda cuando seleccionas el botón
**Guardar cambios** (o **Configurar servidor**). `Esc` cancela. Puedes mover el
cursor con `←→`, `Inicio` y `Fin`, borrar con `Supr`/retroceso y vaciar con `Ctrl+U`.
Las claves y DSN se ocultan. Los borrados de proveedores, modelos y trazas requieren
escribir `ELIMINAR` y confirmar el botón.

Los siguientes atajos siguen disponibles dentro de cada sección; todas esas
acciones también se pueden elegir desde el menú de `Enter`:

| Pantalla | Acciones |
|---|---|
| Proveedores | `n` crear, `e` editar, espacio habilitar, `d` eliminar con confirmación |
| Proveedores | `o` conectar ChatGPT/Zed en navegador, `z` credenciales Zed, `u` desconectar, `K` borrar API key, `m` IDs de models.dev |
| Modelos | `/` buscar, espacio activar/desactivar, `n` ID manual, `e` nombre/protocolo, `d` retirar, `f` predeterminado |
| Modelos | `r` actualizar catálogo y cotejo; los metadatos se consultan en el menú de `Enter` |
| Preferencias | `e` editar modelo, contexto, compactación, timeout y URL pública; `R` restaurar límites |
| Trazas | `e` configurar captura, `n`/`p` paginar, `x` exportar la página visible, `d` borrar con confirmación; inspeccionar desde `Enter` |
| Todas | `s` sincronizar Claude Code, `r` actualizar, `q` salir |

Los tipos de proveedor son `opencode_go`, `openai`, `chatgpt`, `google`,
`openai_compatible` y `zed`. OpenCode Go, OpenAI, Google y Zed tienen endpoints predeterminados;
las APIs propias requieren URL. Cada modelo selecciona explícitamente `chat`,
`responses` o `messages`. ChatGPT utiliza suscripción con Responses; Google utiliza
Chat Completions. La API conserva las claves cuando el formulario envía `null`.

El catálogo coteja IDs exactos con models.dev y obtiene de allí nombres, límites,
precios y capacidades. No descarta modelos desconocidos. Los modelos manuales o
retirados del catálogo permanecen configurables. Los fallos de catálogo no impiden
editar los modelos guardados. Los IDs públicos son `proveedor/modelo`.

OAuth mantiene PKCE, state de un solo uso, nonce, JWKS y renovación en el servidor.
El callback sigue en `http://127.0.0.1:10801/auth/callback`. La conexión ChatGPT se
completa desde el equipo del servidor; el navegador muestra una confirmación y
se vuelve al TUI con `r`. No se necesita una aplicación web.

## Proveedor Zed nativo

La API pública sigue siendo **Anthropic**: `/v1/models`, `/v1/messages` y SSE con
eventos `message_start`, `content_block_*`, `message_delta` y `message_stop`.
Los clientes no necesitan conocer el transporte de Zed. El servidor envía todos
sus modelos a `https://cloud.zed.dev/completions`, incluidos los Claude.
El motor desempaqueta NDJSON y adapta Anthropic Messages, OpenAI/xAI Responses y
Google GenerateContent a ese contrato público. Para `stream:false` reconstruye
el mensaje Anthropic completo a partir de los mismos eventos.

En Proveedores, añade una conexión de tipo **Zed**, guarda y abre su menú con
`Enter`. Elige **Conectar Zed en el navegador**; al terminar, actualiza los datos
y abre **Ver y elegir modelos**. La conexión utiliza una clave RSA efímera y el
callback nativo de Zed en `http://127.0.0.1:<puerto REST>/`. Ese puerto debe ser
accesible desde el equipo del navegador, también con Docker o un túnel remoto.
No se reinstala la UI web; el callback solo devuelve una confirmación de texto.

Como alternativa, **Introducir credenciales de Zed** acepta el ID numérico de
usuario y su token de cuenta, con organización opcional. No acepta una API key
Anthropic ni el token LLM temporal como sustituto. El servidor comprueba la cuenta,
selecciona su organización predeterminada/personal y obtiene un token LLM. Las
credenciales de cuenta se guardan en la base del servidor; los tokens LLM se
mantienen en memoria, se reutilizan y se renuevan una vez cuando Zed los rechaza.
Desconectar elimina las credenciales locales; no revoca la sesión en Zed.

El catálogo se consulta directamente en Zed y conserva el proveedor original
de cada modelo. Los modelos manuales requieren seleccionarlo; la CLI calcula
su protocolo interno automáticamente. Los IDs públicos son `ID-proveedor/ID-modelo`.
Las firmas de razonamiento y el estado de llamadas Google se conservan en
bloques Anthropic de estado opaco para continuar conversaciones con herramientas.
Los tipos de contenido Google que el motor no puede representar se rechazan
explícitamente. Las extensiones `anthropic-beta` no se transmiten por el envoltorio
Zed y generan un aviso de adaptación. `/v1/messages/count_tokens` devuelve una
estimación, identificada mediante `x-claude-proxy-token-count: estimated`.

El adaptador anuncia la versión Zed `1.23.3`; el formulario de credenciales permite
cambiarla si el servicio exige otra. Los fallos del proveedor y los streams sin
final válido producen errores Anthropic. El soporte se verifica con servidores
simulados; no implica una comprobación de tu suscripción o cuota real de Zed.

## Claude Code

```sh
claude-proxy --sync
claude-proxy --dry-run
claude-proxy --sync --endpoint http://127.0.0.1:10801
```

Conserva permisos, plugins y preferencias existentes; actualiza modelos y
`ANTHROPIC_BASE_URL`, crea copias exactas y escribe atómicamente con bloqueo y
detección de ediciones concurrentes. Un catálogo vacío/incorrecto no sobrescribe
`settings.json`. Admite `--settings`, `--default-model`, `--timeout` y `--api-key`.
El destino predeterminado es `$CLAUDE_CONFIG_DIR/settings.json` o
`~/.claude/settings.json`. Reinicia Claude Code después de sincronizar.

## Trazabilidad: issue #2

La captura está desactivada inicialmente. Al activarla, guarda **solo metadatos**:
modelo, protocolo, estado HTTP, tiempo hasta cabeceras, etapas de conversión,
contadores de herramientas/razonamiento/multimedia, tipos de eventos SSE y
adaptaciones esperadas. Cada petición recibe `x-claude-proxy-trace-id`.

Las etapas son `client_request`, `upstream_request`, `upstream_response` y
`client_response`; en streaming, las respuestas se registran como secuencias
`upstream_event`/`client_event`. Se distinguen peticiones inválidas, fallos de
transporte/proveedor, errores de conversión/stream, cancelaciones y finalización.
Los avisos de adaptación no se etiquetan automáticamente como pérdidas. En JSON,
el motor comprueba pérdidas de IDs, nombres, argumentos y cantidad de
herramientas, además del texto Chat; las paradas locales se clasifican como
cambios esperados. En SSE, la máquina de estados detecta herramientas incompletas
y finales ausentes. No es un verificador semántico exhaustivo.

Escribir exactamente `CAPTURAR CONTENIDO` en el formulario concede captura
completa durante una hora. La API admite un vencimiento explícito de hasta 24
horas. El consentimiento caducado vuelve automáticamente a metadatos. Las
cabeceras HTTP nunca se capturan; los campos de credenciales se redactan incluso
con consentimiento. El contenido del usuario puede contener secretos arbitrarios:
la captura completa es deliberadamente explícita.

La retención es de 1–168 horas. Las trazas se guardan siempre en
`server/traces.sqlite3`, también si los ajustes usan PostgreSQL/MySQL. Se purgan al
consultar y al escribir, y cada minuto. Límites: 2.000 trazas, 512 observaciones y
2 MiB por traza; una etapa completa admite hasta 128 KiB. Los eventos descartados
se contabilizan. Una cola acotada de escritura evita bloquear la generación; si se
satura, se registra el descarte en el log. Al finalizar una petición se persiste su
traza, incluidos cancelaciones y streams truncados. Las exportaciones se guardan
en `diagnostics/` dentro de la configuración del CLI.

La visibilidad cubre únicamente cliente ↔ proxy ↔ proveedor. No permite observar
el tráfico interno OpenCode Go ↔ DeepSeek ni demostrar ausencia de pérdidas solo
con contadores. Las pruebas incluyen DeepSeek V4.1 Flash simulado, herramientas
paralelas/subagentes, razonamiento y continuidad entre turnos. Los informes live
anteriores en `docs/` son históricos; esta migración no ejecuta inferencias pagadas.

## Arquitectura y conversión

```text
crates/engine/             biblioteca claude-proxy-engine
  src/translate/          contenido, herramientas, razonamiento y secuencias de parada
  src/stream.rs           decodificador SSE y máquina de traducción incremental
  src/diagnostics.rs      política y observaciones sin contenido sensible
crates/proxy/              ejecutable claude-proxy-server
  src/domain.rs           configuración y puerto SettingsStore
  src/application.rs      casos de uso, validación y snapshots
  src/adapters/           REST, Anthropic, upstream, OAuth, SQL y trazas
crates/cli/                paquete cli; ejecutable claude-proxy
  src/control.rs          descubrimiento, cliente REST y configuración local
  src/tui.rs              panel Ratatui
  src/main.rs             arranque y sincronización de Claude Code
```

El motor no depende de Axum, Reqwest, Tokio, SQL, CLI ni filesystem. Recibe JSON y
bytes SSE y devuelve valores/eventos. El servidor conserva la composición y los
adaptadores de red/persistencia; la aplicación depende del puerto de almacenamiento.
No hay traits por proveedor, registros de conectores ni un framework hexagonal.

Se conservan texto, imágenes/PDFs, resultados y herramientas paralelas, nombres
largos, esquemas estrictos, rechazos, anotaciones, caché y estado de razonamiento
entre turnos. Las funciones sin equivalente se rechazan explícitamente. Los
estados propios del proxy están ligados al modelo público; no se fabrican firmas
Anthropic ni se expone razonamiento como texto público. Consulte la
[matriz de compatibilidad](docs/protocol-compatibility.md).

El SSE admite UTF-8 fragmentado, BOM, LF/CRLF/CR, comentarios y datos multilínea.
Mantiene pings, timeout de inactividad y errores explícitos para truncamientos.
Los límites se aplican a cada evento. Las secuencias de parada funcionan también
cuando se dividen entre eventos y preservan el uso real del proveedor.

## API de administración

| Método | Ruta | Función |
|---|---|---|
| GET | `/api/status` | Identidad, configuración y puerto Anthropic |
| POST | `/api/setup` | Elegir infraestructura inicialmente |
| GET / PUT | `/api/settings` | Configuración con credenciales ocultas |
| GET | `/api/cli-config` | Modelos y preferencias de Claude Code |
| POST | `/api/providers/catalog` | Catálogo y cotejo con models.dev |
| GET | `/api/metadata/providers` | IDs de proveedor de models.dev |
| POST / DELETE | `/api/providers/{id}/oauth` | Conectar/desconectar ChatGPT |
| DELETE | `/api/providers/{id}/api-key` | Borrar API key |
| GET / DELETE | `/api/traces` | Consultar/borrar trazas; `?after=ID` pagina de 100 en 100 |
| GET | `/api/traces/{id}` | Detalle de una traza |

Las mutaciones requieren `Content-Type: application/json`. Se conservan las
guardas de origen y DNS rebinding. La administración remota admite hosts IP y
localhost; para un dominio propio añade `--admin-host proxy.example.com` al
servidor. Expón el puerto solo a una red de confianza. Los puertos son
configurables mediante `--port` y `--admin-port` (`--ui-port` es alias legado).
Las credenciales se guardan sin cifrado de aplicación; los archivos de servidor
usan directorio 0700 y archivos sensibles 0600 en Unix.

## Docker y comprobaciones

```sh
docker compose up -d --build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Docker publica ambos puertos en loopback y guarda infraestructura y trazas en el
volumen `proxy-data`. El panel se ejecuta en el terminal del host y se conecta al
puerto 10801. No hay etapa de compilación frontend.

La prueba de bases remotas requiere dos bases **desechables**:

```sh
PROXY_TEST_POSTGRES=postgres://usuario:clave@host/base \
PROXY_TEST_MYSQL=mysql://usuario:clave@host/base \
cargo test -p claude-proxy-server remote_database_roundtrips -- --ignored
```

El informe de validación de esta migración está en
[docs/migration-verification.md](docs/migration-verification.md).
