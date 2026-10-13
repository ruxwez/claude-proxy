# Verificación de la migración a motor + servidor + CLI

Fecha: 13 de octubre de 2026 (Europe/Madrid).

## Entorno

Rust 1.99 en Linux, mediante contenedores Docker desechables. La compilación
nativa del host Windows está bloqueada por su directiva de Control de aplicaciones
(error 4551, incluidos build scripts y herramientas Rust); no se ha validado un
binario Windows. Se conserva el soporte multiplataforma de Rust, Ratatui/Crossterm
y directories, y los flags específicos Unix están protegidos con `cfg(unix)`.

## Resultados

- `cargo build --workspace --locked`: correcto.
- `cargo fmt --all -- --check`: correcto.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: correcto.
- `cargo test --workspace --locked -- --include-ignored`: **78 pruebas correctas**,
  cero fallos y cero omitidas, proporcionando las dos bases remotas de prueba.
  Motor: 18; servidor: 50; CLI: 10.
- PostgreSQL 17 y MySQL 8.4 reales: creación de esquema, persistencia de
  credenciales, documento de más de 64 KiB y reapertura correctos.
- `python3 scripts/verify_tui.py --bin-dir /build/debug`: correcto en un PTY de
  120 × 35. Recorre configuración inicial, proveedor con clave oculta, modelo
  manual, sincronización de Claude Code, captura de trazas, reinicio y
  restauración del terminal. Todo se ejecuta contra datos temporales y un
  proveedor HTTP simulado.
- `docker compose config --quiet` y `git diff --check`: correctos.

La prueba remota se omite en la suite ordinaria si no se pide explícitamente
`--include-ignored` o `--ignored`. Sus DSN deben apuntar a bases desechables.

### Navegación del TUI con flechas y Enter

Revisión posterior del panel, tomando como referencia la interacción de
`quatum-migrator`: menú lateral, acciones contextuales, selectores, botones
explícitos y ayuda. Se verifican por separado estas modificaciones de la CLI:

- `cargo test -p cli --locked`: 13 pruebas correctas, incluyendo navegación sin
  guardar accidentalmente, cancelación de menús/ayuda y edición Unicode de claves.
- `cargo clippy -p cli --all-targets --locked -- -D warnings` y formato: correctos.
- `scripts/verify_tui.py`: recorrido real en PTY usando flechas y Enter, sin
  atajos de letras para añadir proveedor, modelo, sincronizar o configurar trazas.
  Comprueba además selección de protocolo, credenciales ocultas, persistencia
  tras reinicio y restauración del terminal.

Estas comprobaciones se ejecutan en Linux y no sustituyen la validación nativa
de Windows pendiente por la limitación del host descrita arriba.

### Adaptador Zed nativo

Verificado el 13 de octubre de 2026 en el contenedor desechable de validación,
con endpoints HTTP simulados y una base SQLite temporal:

- `cargo test --workspace --locked`: 92 pruebas correctas (26 motor, 53 servidor,
  13 CLI); una prueba de PostgreSQL/MySQL omitida porque requiere sus DSN desechables.
- Conversión de Anthropic, OpenAI, xAI y Google desde `/completions` a
  `/v1/messages`, tanto JSON como SSE; IDs públicos, firmas, herramientas y caché.
- NDJSON fragmentado en cada byte, Unicode, CRLF, BOM, estados de cola/fallo,
  límites por línea y ausencia de `stream_ended`. Un corte no emite `message_stop`.
- Intercambio de token por organización, renovación tras 401, reutilización y
  ocho peticiones simultáneas compartiendo una única renovación.
- Autenticación nativa con RSA/OAEP, formato PKCS1/base64url del enlace Zed,
  callback de un solo uso, rechazo de callbacks ajenos y almacenamiento persistente.
- API de administración sin credenciales, conservación frente a un roundtrip
  manipulado, descubrimiento de catálogo y desconexión durable.
- Estado Google opaco con firmas y IDs de llamadas, partes de firma aisladas,
  rechazo explícito de controles no representables y observaciones sin contenido.
- Clippy con `-D warnings`, formato y build del workspace correctos.
- PTY real: alta Zed, credencial oculta, conexión, catálogo, activación de Claude,
  protocolo interno automático, reinicio y restauración del terminal mediante
  flechas y Enter. Continúan pasando los recorridos anteriores del panel.

La comprobación no utiliza una cuenta real de Zed ni consume cuota. No confirma
la disponibilidad de modelos en una suscripción concreta. El contenedor de
producción no se sustituye durante estas pruebas.

### Regresión del esquema Anthropic de Zed

El error real `unknown variant system, expected user or assistant` mostró que
el simulador aceptaba JSON que los structs de Zed rechazan. Se restringe ahora
el fixture a esos dos roles y a contenido como lista de bloques. El adaptador
mueve los mensajes `system` al campo superior, conserva el sistema existente y
sus marcadores de caché, convierte texto directo en bloques y completa los
campos requeridos por Zed en herramientas/resultados. Los roles desconocidos se
rechazan antes de enviar la petición. El comportamiento de proveedores Messages
ajenos a Zed permanece igual.

La suite completa pasa con 94 pruebas (27 motor, 54 servidor, 13 CLI), una prueba
remota omitida; también pasan Clippy y formato. El caso HTTP incluye instrucciones
mixtas y contenido de texto directo tanto en JSON como en SSE.

Tras reconstruir y sustituir el contenedor Compose conservando `proxy-data`,
se verificaron `/health` y `/api/status`. Una petición real a
`zed/claude-haiku-5-5`, con `system` superior y otro mensaje de rol `system`,
texto directo y límite de 32 tokens, devolvió HTTP 200, un mensaje Anthropic,
texto `OK` y `stop_reason: end_turn` (35 tokens de entrada, 4 de salida).
La imagen previa se conserva como `claude-proxy:before-zed-request-fix`.

## Regresiones y mejoras cubiertas

Se mantienen las pruebas existentes de razonamiento, herramientas paralelas y
continuaciones de subagentes, esquemas, rechazos, imágenes/PDFs, caché,
preferencias, claves, OAuth, migraciones y errores de transporte. La separación
no cambia los IDs públicos de modelos ni el contrato Anthropic.

Se añaden casos de BOM, líneas CR, CRLF fragmentado, UTF-8 fragmentado, eventos
sin dos puntos y chunks de red con más de 4 MiB que contienen eventos pequeños.
El límite sigue protegiendo cada línea/evento. El motor puede procesar también
los eventos Messages nativos y rechazar su truncamiento sin depender de HTTP.

Las trazas se verifican con DeepSeek V4.1 Flash simulado en Chat JSON y SSE.
Capturan las cuatro fronteras y el estado del stream sin persistir prompts,
respuestas, razonamiento ni claves por defecto. Se verifican consentimiento
caducado, redacción, límites de eventos, expiración, paginación con marcas de
tiempo idénticas y distinción entre pérdidas comprobables de herramientas y
filtrado deliberado por secuencias de parada. Los hosts DNS administrativos
solo se aceptan si se han permitido explícitamente en el servidor.

La prueba real de MySQL detectó su representación `LONGTEXT` como BLOB en SQLx
Any. Se corrigió la decodificación UTF-8 y se eliminó el acceso a columnas que
podía causar un panic. Las operaciones remotas tienen timeout y un pool que
puede restablecer conexiones; las escrituras se ejecutan fuera de los workers
async de red.

## Límites de esta verificación

No se han ejecutado inferencias pagadas ni un nuevo inicio de sesión real en
OpenAI. Los informes anteriores `reasoning-verification.*` y
`protocol-verification.json` son históricos y no acreditan esta migración.

Los contadores y las comprobaciones estructurales no demuestran equivalencia
semántica completa. La traza no observa el tráfico interno entre OpenCode Go y
DeepSeek. El siguiente diagnóstico debe utilizar trazas recogidas durante uso
real, distinguiendo adaptaciones declaradas, fallos de transporte y pérdidas
confirmadas. La captura completa sigue siendo opcional y temporal.
