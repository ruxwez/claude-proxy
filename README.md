# Claude Proxy

Monorepo Rust con motor de conversión independiente, servidor hexagonal y panel
TUI con Ratatui. No requiere Node ni incluye una UI web.

## Instalación y conexión

Descarga el paquete de tu plataforma de [Releases](https://github.com/ruxwez/claude-proxy/releases)
y extrae `claude-proxy` y `claude-proxy-server` juntos. En Linux/macOS marca ambos
como ejecutables (`chmod +x claude-proxy claude-proxy-server`) y ejecuta el CLI:

```sh
./claude-proxy
```

El CLI comprueba primero la conexión guardada. Si falla, comprueba exclusivamente
`127.0.0.1` y `::1` en el puerto REST estándar **10801**, con timeout de dos
segundos por intento, sin utilizar proxies HTTP para loopback. No escanea la LAN ni utiliza broadcast, mDNS o subredes.
Ofrece reutilizar un servidor local encontrado; si no hay uno funcional, puedes
**instalar el servidor local** o **introducir un endpoint y una clave opcional**.

La instalación local copia el servidor a un directorio estable, fuerza SQLite,
registra un servicio del sistema, activa el arranque automático, lo inicia y
comprueba su disponibilidad antes de guardar la conexión. Linux utiliza systemd,
macOS launchd y Windows un Windows Service nativo. Solo las operaciones que
modifican servicios solicitan sudo/UAC. El servicio escucha en **127.0.0.1**.
Las reinstalaciones reutilizan el ejecutable y los datos existentes.

El paquete distribuido incluye ambos ejecutables para instalar sin herramientas
de compilación ni descargar nada más. Si solo tienes el CLI, busca el servidor
junto a él o en PATH y, como alternativa, descarga el binario y su SHA-256 de la
release de la misma versión. En repositorios privados descarga el paquete con
una cuenta autorizada: el CLI no solicita ni guarda un token de GitHub.

Para compilar e instalar desde el repositorio (Rust 1.96 y compilador C):

```sh
cargo build --workspace --release --locked
cargo install --locked --path crates/proxy
cargo install --locked --path crates/cli
claude-proxy
```

También puedes ejecutar `claude-proxy-server` directamente, sin CLI ni Docker.
Los proveedores y modelos se configuran desde el panel; después utiliza su acción
**Sincronizar modelos con Claude Code**, como antes.

| Servicio | Endpoint predeterminado |
|---|---|
| Gateway Anthropic y administración | `http://127.0.0.1:10800` (`/v1/*`, `/api/*`) |
| Puerto REST conservado | `http://127.0.0.1:10801/api/*` |
| Salud sin autenticación | `http://127.0.0.1:10800/health` |

Los servidores LAN/remotos se introducen manualmente, por ejemplo
`https://proxy.example.com` o `http://192.168.1.20:10800`. El CLI nunca descubre
otros equipos. `--endpoint`/`CLAUDE_PROXY_URL` seleccionan un servidor explícito;
`CLAUDE_PROXY_API_KEY` permite suministrar la clave sin ponerla en los argumentos.

## Bases de datos y autenticación

El servidor se configura e inicializa automáticamente al arrancar:

- Sin `DATABASE_URL`: SQLite persistente en `server/proxy.sqlite3`.
- `sqlite:///ruta/absoluta.sqlite3` o `sqlite://archivo.sqlite3`: SQLite persistente.
- `postgres://usuario:clave@host/base` / `postgresql://...`: PostgreSQL mediante SQLx.
- `mysql://usuario:clave@host/base`: MySQL mediante SQLx.

Las bases externas deben existir; se crean las tablas y se aplican las migraciones
al arrancar. `SettingsStore` mantiene un único contrato de persistencia y los
mismos casos de uso para los tres motores. La configuración sigue siendo un
documento JSON transaccional; no se añaden ORM ni tablas de negocio redundantes.
Las migraciones del documento y de OpenAI/ChatGPT conservan IDs y credenciales.
Solo debe haber un servidor por base: los snapshots son locales al proceso.

`--database /ruta/existente.sqlite3` tiene prioridad y fuerza SQLite (el CLI lo
utiliza para instalaciones locales). Después se aplica `DATABASE_URL`, luego el
archivo legado `server/database.json`, la base antigua `claude-proxy.sqlite3` y,
finalmente, SQLite predeterminado. Se conservan las bases antiguas sin copiarlas
mientras están abiertas. Cambiar el motor no copia datos entre bases: realiza una
migración o restauración explícita. El CLI ya no ofrece seleccionar un motor.

Sin `AUTH_KEY`, las APIs no exigen clave. Si está definida, protege todas las
rutas `/api/*` y `/v1/*` en ambos puertos. Es una única clave compartida, sin
usuarios, JWT de acceso, OAuth de acceso ni sesiones. Se compara un digest de
longitud fija en tiempo constante; una clave vacía definida se rechaza.

Se conserva la inyección existente de `ANTHROPIC_API_KEY` en Claude Code, enviada
como `X-Api-Key`. El servidor también admite `ANTHROPIC_AUTH_TOKEN` mediante
`Authorization: Bearer`. Si llegan ambos headers, Authorization tiene prioridad.
Esto sigue la [documentación oficial de Claude Code](https://code.claude.com/docs/en/llm-gateway-connect#how-the-credential-variable-maps-to-a-header).
Ambas cabeceras de acceso se eliminan antes del gateway; cada proveedor usa
exclusivamente sus propias credenciales. `/health` y los callbacks de navegador
siguen accesibles; estos últimos conservan sus estados de un solo uso, PKCE o
challenge RSA. No hay un protocolo nuevo de autenticación de cliente.

## Configuración del CLI y servicio local

La sección **Conexión** del panel permite ver el endpoint, cambiarlo, configurar
una clave oculta, eliminarla, comprobar conectividad y gestionar el servicio de
**este equipo**, aunque la conexión seleccionada sea remota. Cambiar de conexión
no mueve ni borra datos del servidor. Si el servidor tiene modelos activos, cambiar
la conexión desde el panel también sincroniza Claude Code.

```sh
claude-proxy config show
claude-proxy config connect https://proxy.example.com
claude-proxy config key            # solicita la clave sin mostrarla
claude-proxy config key --clear
claude-proxy config check
claude-proxy service install       # local, siempre SQLite y loopback
claude-proxy service start
claude-proxy service stop
claude-proxy service restart
claude-proxy service status
```

El endpoint se guarda atómicamente en la carpeta habitual de `directories`:
Windows `%APPDATA%/claude-proxy/config/config.json`, Linux
`$XDG_CONFIG_HOME/claude-proxy/config.json` o `~/.config/claude-proxy/config.json`,
macOS `~/Library/Application Support/claude-proxy/config.json`.
`CLAUDE_PROXY_CONFIG_DIR` permite aislar la configuración del CLI.

La clave se guarda preferentemente en Credential Manager (Windows), Keychain
(macOS) o Secret Service (Linux). Si no están disponibles, utiliza un archivo
separado protegido con permisos 0600/directorio 0700 en Unix o ACL privada en
Windows. `config.json` contiene el endpoint y la referencia al almacén, nunca la
clave. El panel y `config show` solo indican si está configurada. Claude Code
recibe la clave en su `settings.json`, como en la integración existente; ese
archivo y sus copias se protegen en Unix. No añadas esos archivos a repositorios.

Los datos del servicio local están en `server/` dentro de la carpeta habitual;
en Windows se utilizan `%ProgramData%/ClaudeProxy`, con permisos de LocalService.
Los ejecutables gestionados se guardan en su subdirectorio `bin/`. La instalación
no reemplaza un binario existente en uso. Para actualizarlo, detén el servicio,
sustituye el binario gestionado y vuelve a iniciarlo; los datos se conservan.

Las instrucciones completas de Docker, ejecución nativa y servicios están en
[docs/deployment.md](docs/deployment.md).

## Panel de terminal

El panel tiene un menú lateral: elige Proveedores, Modelos, Preferencias o Trazas
con `↑↓` y abre la sección con `Enter`. Dentro, selecciona un elemento con `↑↓`
y pulsa `Enter` para ver sus acciones. El menú incluye añadir, editar, activar,
consultar y sincronizar con Claude Code; también funciona cuando la lista está vacía.
`Esc` vuelve al nivel anterior y `F1` muestra la ayuda.

En formularios, `↑↓` o `Tab` cambian de campo. Los tipos de proveedor, protocolos,
opciones Sí/No se eligen con `←→`; no hay que escribir sus nombres.
`Enter` avanza al siguiente campo: solo guarda cuando seleccionas el botón
**Guardar cambios**. `Esc` cancela. Puedes mover el
cursor con `←→`, `Inicio` y `Fin`, borrar con `Supr`/retroceso y vaciar con `Ctrl+U`.
Las claves se ocultan. Los borrados de proveedores, modelos y trazas requieren
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
El CLI recibe el callback en un puerto loopback temporal y lo envía autenticado
al servidor. Funciona también con servidores remotos. El intercambio de tokens
se completa en el servidor; el navegador muestra una confirmación y
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
callback nativo de Zed en `http://127.0.0.1:<puerto temporal del CLI>/`. El CLI
lo reenvía autenticado al servidor, también con Docker o servidores remotos.
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
  src/installation.rs     instalación automática y descarga verificada
  src/callback.rs         callbacks loopback hacia el servidor remoto
crates/service/            systemd, launchd y Windows Service nativo
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
| POST | `/api/setup` | Compatibilidad heredada; un servidor iniciado ya está configurado |
| GET / PUT | `/api/settings` | Configuración con credenciales ocultas |
| GET | `/api/cli-config` | Modelos y preferencias de Claude Code |
| POST | `/api/providers/catalog` | Catálogo y cotejo con models.dev |
| GET | `/api/metadata/providers` | IDs de proveedor de models.dev |
| POST / DELETE | `/api/providers/{id}/oauth` | Conectar/desconectar ChatGPT |
| DELETE | `/api/providers/{id}/api-key` | Borrar API key |
| GET / DELETE | `/api/traces` | Consultar/borrar trazas; `?after=ID` pagina de 100 en 100 |
| GET | `/api/traces/{id}` | Detalle de una traza |

Las APIs usan la clave compartida si `AUTH_KEY` está definida. Las mutaciones requieren `Content-Type: application/json`. Se conservan las
guardas de origen y DNS rebinding. La administración remota admite hosts IP y
localhost; con `AUTH_KEY` permite también dominios autenticados. Sin clave, para un dominio
propio añade `--admin-host proxy.example.com` al servidor. Expón el puerto solo a una red de confianza. Los puertos son
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

Docker publica un puerto en loopback de forma predeterminada y guarda los datos
en `proxy-data`. Ese endpoint sirve tanto `/v1/*` como `/api/*`; el CLI se ejecuta
en el equipo del usuario. El puerto y la interfaz publicados son configurables.
Consulta [despliegue](docs/deployment.md) y el nuevo
[informe de verificación](docs/architecture-verification.md).

La prueba de bases remotas requiere dos bases **desechables**:

```sh
PROXY_TEST_POSTGRES=postgres://usuario:clave@host/base \
PROXY_TEST_MYSQL=mysql://usuario:clave@host/base \
cargo test -p claude-proxy-server remote_database_roundtrips -- --ignored
```

El informe de validación de esta migración está en
[docs/migration-verification.md](docs/migration-verification.md).
