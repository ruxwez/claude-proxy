# Instalación y despliegue

Los tres escenarios ejecutan el mismo `claude-proxy-server`. No hay servidor
especial para el CLI ni para Docker; la conversión, streaming, proveedores,
modelos, OAuth y trazas comparten exactamente los mismos adaptadores.

## 1. Usuario local: instalación automática desde el CLI

Descarga y extrae el ZIP de tu plataforma desde las releases autorizadas del
repositorio. Incluye el CLI y el servidor. En Linux/macOS:

```sh
chmod +x claude-proxy claude-proxy-server
./claude-proxy
```

En Windows ejecuta `claude-proxy.exe`. El asistente comprueba la conexión guardada
y luego localhost:10801. Si no hay servidor, selecciona **Instalar servidor local**.
Copia el ejecutable a un directorio estable, prepara SQLite, registra el servicio,
activa su inicio automático, lo inicia y guarda el endpoint cuando responde.
Solicita sudo/UAC para registrar/controlar el servicio. Nunca ofrece MySQL o
PostgreSQL ni escucha en la LAN. No elimina bases existentes.

Después añade tus proveedores y modelos en el panel y selecciona **Sincronizar
modelos con Claude Code**. Se conserva el mecanismo existente de settings,
modelPicker, alias y copias de seguridad. Si ya hay modelos en el servidor, puedes
sincronizar directamente. Reinicia Claude Code para cargar sus settings nuevos.

```sh
claude-proxy service status
claude-proxy service stop
claude-proxy service start
claude-proxy service restart
claude-proxy service update
```

Linux necesita systemd como gestor del equipo; un contenedor/WSL sin systemd no
puede registrar este servicio. La ejecución directa sigue disponible. macOS
registra un LaunchDaemon del sistema bajo el usuario instalador; Windows utiliza
LocalService y su dispatcher nativo (no una tarea programada ni un proceso suelto).
En Linux/macOS el servicio también arranca sin una sesión interactiva abierta.
Los servicios de Windows, launchd y systemd tienen recuperación tras fallos.

Para compilar en lugar de descargar, instala Rust y un compilador C (Build Tools
MSVC en Windows, Xcode Command Line Tools en macOS, build-essential/pkg-config en
Linux), después:

```sh
cargo build --workspace --release --locked
# Ambos binarios quedan juntos en target/release.
./target/release/claude-proxy
```

También puedes instalar ambos paquetes con `cargo install --locked --path ...`,
como describe el README. Docker no interviene en este flujo.

## 2. Servidor remoto con Docker Compose

Necesitas Docker con Compose v2.24 o posterior. En el VPS:

```sh
git clone https://github.com/ruxwez/claude-proxy.git
cd claude-proxy
cp .env.example .env
# Edita .env y define AUTH_KEY; opcionalmente DATABASE_URL.
docker compose up -d --build --wait
```

Sin `DATABASE_URL`, los ajustes y trazas se guardan en el volumen `proxy-data`.
`docker compose down` conserva ese volumen; `down -v` lo elimina. No añadas `-v`
si quieres conservar proveedores, tokens, modelos y trazas.

Por defecto se publica `127.0.0.1:10800`, adecuado para un reverse proxy TLS o
un túnel SSH. El mismo puerto sirve `/api/*`, `/auth/callback`, `/v1/*` y `/health`.
No necesitas publicar 10801 para conectar el CLI.

Para publicar directamente en la interfaz del VPS, define en `.env`:

```dotenv
PROXY_BIND_ADDRESS=0.0.0.0
PROXY_EXPOSE_PORT=10800
AUTH_KEY=una-clave-compartida-larga
```

Conecta el CLI a `http://IP_DEL_VPS:10800`. Para acceso por Internet utiliza un
reverse proxy HTTPS y un endpoint como `https://proxy.example.com`; encaminando
**todas** las rutas al puerto 10800, administración y generación utilizan la misma
URL. Los streams necesitan buffering desactivado y un timeout apropiado en ese
reverse proxy. No se incluye un servicio de reverse proxy obligatorio en Compose.
Si el endpoint público difiere del endpoint del CLI, define `PROXY_PUBLIC_URL`.

Los motores externos no se añaden como servicios de Compose. Utiliza una base
existente y establece uno de estos DSN en `.env`:

```dotenv
DATABASE_URL=postgresql://usuario:clave@db.example:5432/proxy?sslmode=require
# O bien:
DATABASE_URL=mysql://usuario:clave@db.example:3306/proxy?ssl-mode=REQUIRED
```

La base debe existir y ser accesible desde el contenedor; `localhost` dentro del
contenedor es el propio contenedor. El usuario necesita permisos DDL y DML para
las tablas `settings` y `schema_migrations`. Configura TLS conforme a SQLx y al
servidor de base de datos. Las trazas de diagnóstico conservan su almacenamiento
SQLite local en `/data/traces.sqlite3`, igual que antes; mantén el volumen también
cuando los ajustes utilizan un motor externo.

El contenedor ejecuta el servidor como UID 10001, conserva señales de parada y
comprueba `/health` sin enviar ninguna credencial. La imagen final contiene solo
el servidor, certificados y curl. Las variables opcionales deben **omitirse**
si no se utilizan: `AUTH_KEY=` o `DATABASE_URL=` no desactivan esas opciones.

## 3. Servidor remoto nativo, sin Docker

Descarga el binario independiente para Windows, Linux o macOS desde una release
autorizada, verifica su archivo SHA-256 y renómbralo como `claude-proxy-server`
(`.exe` en Windows). Para compilar solo el servidor:

```sh
cargo build --release --locked -p claude-proxy-server
```

SQLite va incluido: no necesitas instalar sus bibliotecas ni un servidor de base
de datos. PostgreSQL y MySQL solo requieren una conexión a la base externa.

Bash/Zsh:

```sh
export PROXY_HOST=0.0.0.0
export PROXY_CONFIG_DIR=/ruta/persistente/claude-proxy
# Opcionales; introduce la clave sin añadirla al historial del shell.
printf 'AUTH_KEY: '
IFS= read -r -s AUTH_KEY
export AUTH_KEY
printf '\n'
# export DATABASE_URL='postgresql://usuario:clave@db.example/proxy?sslmode=require'
./claude-proxy-server
```

PowerShell:

```powershell
$env:PROXY_HOST = '0.0.0.0'
$env:PROXY_CONFIG_DIR = 'C:\ClaudeProxyData'
$secret = Read-Host 'AUTH_KEY' -AsSecureString
$env:AUTH_KEY = [Net.NetworkCredential]::new('', $secret).Password
# $env:DATABASE_URL = 'mysql://usuario:clave@db.example/proxy'
.\claude-proxy-server.exe
```

En ambos casos, omite la lectura de clave si quieres un servidor sin autenticación.
El CLI se conecta al endpoint de ese servidor; Docker no es necesario.

### Registrar el servidor nativo como servicio

El binario ofrece el mismo gestor de servicios en las tres plataformas:

```sh
claude-proxy-server service install --config-dir /ruta/absoluta/persistente --host 0.0.0.0
claude-proxy-server service status
claude-proxy-server service stop
claude-proxy-server service start
claude-proxy-server service restart
```

En Windows utiliza rutas como `C:\ClaudeProxyData` y `claude-proxy-server.exe`.
No muevas ni borres el ejecutable después de registrar el servicio. Este comando
registra el binario actual; el CLI instala una copia estable automáticamente.
Mantén sus rutas fuera de directorios temporales. El servicio usa un nombre fijo
por equipo (`claude-proxy` / `com.claude-proxy.server`) para ser idempotente.

Las variables del shell no se heredan automáticamente al arrancar con el SO.
Para definir `AUTH_KEY`, `DATABASE_URL`, `PROXY_PUBLIC_URL` u otras variables del
servidor, crea un JSON privado y pásalo al instalador:

```json
{
  "AUTH_KEY": "clave-compartida-del-servidor",
  "DATABASE_URL": "postgresql://usuario:clave@db.example/proxy?sslmode=require",
  "RUST_LOG": "info"
}
```

```sh
chmod 600 servicio.json
claude-proxy-server service install --config-dir /ruta/absoluta/persistente --host 0.0.0.0 --env-file servicio.json
```

En Windows protege el JSON con una ACL de tu cuenta antes de utilizarlo. Linux
instala las variables en `/etc/claude-proxy.env` (0600, root); launchd las guarda
en el plist del sistema (0600); Windows las guarda en `service-env.json` con ACL
para el instalador, administradores, SYSTEM y LocalService. No se imprimen sus
valores. Puedes borrar el JSON fuente después de registrar el servicio.

`--port` y `--admin-port` permiten cambiar los puertos durante el registro;
`--host` y `--config-dir` son argumentos estables del servicio y tienen prioridad
sobre las variables equivalentes. El instalador reinicia el servicio. En Linux,
si reinstalas sin `--env-file`, conserva el EnvironmentFile existente; para
cambiarlo vuelve a pasar un JSON. Las instalaciones locales desde el CLI excluyen
ese archivo y fuerzan `--database`, evitando heredar una base remota.

## Variables compartidas entre Docker y ejecución nativa

| Variable | Valor predeterminado / efecto |
|---|---|
| `DATABASE_URL` | SQLite persistente si no se define; esquema sqlite, postgres/postgresql o mysql |
| `AUTH_KEY` | Sin autenticación si no se define; clave compartida no vacía si se define |
| `PROXY_HOST` | `127.0.0.1` nativo; `0.0.0.0` dentro de Docker |
| `PROXY_PORT` | `10800`, gateway + administración |
| `PROXY_ADMIN_PORT` | `10801`, puerto REST conservado |
| `PROXY_CONFIG_DIR` | Carpeta convencional `claude-proxy/server`; `/data` en Docker |
| `PROXY_ADMIN_HOST` | Lista de dominios administrativos separada por comas, necesaria sin clave para hosts DNS propios |
| `PROXY_PUBLIC_URL` | URL Anthropic pública explícita; si falta se deriva de la conexión del CLI |
| `RUST_LOG` | `info`; nunca incluye claves entrantes |

El ejecutable no carga `.env` automáticamente; Compose sí carga su `env_file`.
Los flags explícitos tienen prioridad sobre sus variables correspondientes.
No se configuran claves de proveedor mediante estas variables: se conservan la
API y el panel existentes de proveedores.

## Conectar el CLI desde otro equipo

Ejecuta `claude-proxy`, elige **Conectar a servidor existente**, introduce el
endpoint y la clave opcional (entrada oculta). También puedes usar:

```sh
claude-proxy config connect https://proxy.example.com
claude-proxy config check
claude-proxy --sync
```

`config connect` guarda una conexión tras validarla; `--sync` conserva el flujo
actual de sincronización. El panel sincroniza automáticamente cuando cambias
la conexión y hay modelos activos. Puedes añadir, reemplazar o eliminar la clave
posteriormente con `config key` o **Conexión**, sin repetir el asistente ni
reinstalar. Las cuentas ChatGPT/Zed usan un callback temporal en loopback del
cliente que reenvía el resultado al servidor; mantén abierto el CLI durante la
conexión. Los tokens del proveedor y la validación siguen en el servidor.

Las copias de seguridad contienen credenciales: protege tanto el directorio de
datos del servidor como los settings de Claude Code. No hay migración implícita
entre bases ni eliminación automática de datos al cambiar de endpoint.
