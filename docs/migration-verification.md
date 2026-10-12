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
