# Verificación de servidor independiente y CLI

Fecha: 13 de octubre de 2026. Entorno: Linux x86_64, Rust 1.96,
Docker 28.4, PostgreSQL 16 y MySQL 8.4 desechables.

## Análisis previo y decisiones

El repositorio ya separaba `claude-proxy-server`, `claude-proxy` y el motor.
La aplicación dependía de `SettingsStore`; SQLite utilizaba rusqlite y las bases
externas SQLx Any. Se conserva ese contrato y el documento transaccional, evitando
migrar a otro ORM o duplicar casos de uso. El servidor deja de esperar un bootstrap
por CLI y selecciona/inicializa su almacenamiento al arrancar.

La autenticación del gateway usa el `X-Api-Key` que ya inyectaba el CLI, además de
aceptar Bearer conforme a la [documentación de Claude Code](https://code.claude.com/docs/en/llm-gateway-connect#how-the-credential-variable-maps-to-a-header).
Los JWT que ya existían solo validan tokens del proveedor ChatGPT; no hay un
sistema de JWT de acceso al proxy. La instalación del SO se comparte en un crate
pequeño entre los dos ejecutables. El motor de conversión no participa en la
instalación ni en la autenticación.

Ambos puertos conservan sus contratos. El gateway también admite administración
para que Docker/NAT/reverse proxies puedan publicar un único endpoint. La API
indica si la conexión recibida sirve ambas funciones; el CLI conserva así el
puerto externo, esquema HTTPS y prefijo de ruta. `PROXY_PUBLIC_URL` permite una
URL pública explícita. Los callbacks OAuth/Zed se reciben en loopback del equipo
del CLI y se reenvían a la validación existente del servidor.

Los clientes HTTP usan el almacén de certificados del SO. Esto permite endpoints
HTTPS con CA corporativas instaladas, sin desactivar TLS; también permite ejecutar
las pruebas con el proxy y CA del entorno gestionado.

## Comprobaciones ejecutadas

- Compilación de los ejecutables del workspace en Linux.
- Formato Rust y Clippy del workspace, incluyendo tests, con warnings como errores.
- Suite del workspace: 101 pruebas correctas en Linux (27 motor, 57 servidor, 14 CLI,
  3 servicios); la prueba remota se ejecuta por separado con sus dos DSN.
- PostgreSQL 16 y MySQL 8.4 reales: esquema inicial, persistencia de claves y
  documento grande, reapertura y migración v1 idempotente.
- `scripts/verify_architecture.py` contra procesos reales con SQLite,
  PostgreSQL y MySQL: inicialización sin CLI, autenticación de APIs y generación,
  aislamiento de la clave del gateway frente al proveedor, sincronización real
  de settings/modelos de Claude Code, conexión y clave recordadas, eliminación
  de clave, rechazo sin modificar settings y persistencia tras reiniciar.
- `scripts/verify_tui.py` en PTY real: flechas/Enter, configuración automática
  de SQLite, proveedor y clave oculta, modelo/protocolo, sincronización, trazas,
  cuenta/catálogo/modelo Zed, reinicio y restauración del terminal.
- Compilación cruzada de `claude-proxy-service` para Windows x86_64 GNU
  y macOS x86_64, incluyendo dispatcher, control de parada, registro/UAC
  y definiciones launchd.
- Unidad systemd comprobada con su parser real, incluyendo rutas con espacios.
- launchd en una VM macOS de CI: instalación, arranque automático, reinstalación
  sin perder la identidad persistida, inicio, parada, reinicio y estado correctos.
- Paquete Linux release CLI + servidor y SHA-256 verificados.
- Construcción real de la imagen Docker, ejecución sin root, `/health` y
  health check correcto; API sin clave rechazada con 401.
- Prueba real autorizada de OpenCode Go: catálogo de 38 modelos y petición a
  `deepseek-flash` con 32 tokens máximos, HTTP 200, mensaje Anthropic válido,
  `stop_reason: end_turn`. Se utilizó una base temporal y no se conservó la clave
  en el repositorio ni en logs.

Las pruebas nuevas del servidor verifican URI, headers nativos, precedencia
Bearer, key opcional, protección de ambos listeners y salud pública. El CLI
verifica el relay de callback hacia la API autenticada sin exponer el código o
la clave al navegador. Se conservan las pruebas previas de razonamiento,
herramientas, SSE, protocolos, suscripción, Zed y redacción de credenciales.

## Repetir las pruebas

```sh
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/verify_architecture.py --bin-dir target/debug
python3 scripts/verify_tui.py --bin-dir target/debug
```

Para incluir bases externas, define `PROXY_TEST_POSTGRES` y `PROXY_TEST_MYSQL`
apuntando a bases **desechables**, tanto para el script como para:

```sh
cargo test -p claude-proxy-server --locked remote_database_roundtrips -- --ignored
```

## Plataformas y límites

El entorno actual no ejecuta Windows/macOS ni tiene systemd como PID 1. No se
presenta la compilación cruzada como una prueba de arranque del servicio Windows.
`ci.yml` ejecuta build, tests, Clippy y procesos reales en Windows, macOS y Linux;
`verify_service.py` registra, reinstala, inicia, detiene, reinicia y consulta el
servicio **solo en las VM desechables de CI**. No lo ejecutes sobre un equipo con
un servicio de producción instalado bajo el mismo nombre.

`release.yml` construye los seis binarios nativos (x86_64/aarch64 por SO), el
servidor independiente con SHA-256 y paquetes CLI + servidor. Las releases son
borradores hasta su publicación. La descarga automática de un CLI aislado necesita
una release publicada y accesible; el paquete completo instala sin descargas.
La disponibilidad de assets privados depende de los permisos de GitHub del usuario.

No se ha realizado un nuevo login real de ChatGPT o Zed; los callbacks y estados
se prueban con fixtures. La prueba de OpenCode Go valida una petición actual,
no garantiza cuotas futuras ni todos los modelos de una suscripción.
Las trazas mantienen SQLite local como almacenamiento diagnóstico, también con
ajustes en PostgreSQL/MySQL. No se incluye migración de datos entre motores ni
coordinación de snapshots entre varios procesos del servidor.
