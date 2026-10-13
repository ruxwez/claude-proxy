# Publicación de CLI y servidor

`.github/workflows/release.yml` publica una única release por versión, con el CLI
y el servidor juntos para Linux, Windows y macOS, en x86_64 y aarch64. Cada ZIP
incluye ambos ejecutables; la misma release contiene también los servidores
individuales que descarga automáticamente el CLI. ZIP y servidor tienen SHA-256.
No hay tags ni releases separados para CLI y servidor.

## Configuración inicial

Configura en GitHub Actions el secreto del repositorio `CARGO_REGISTRY_TOKEN`,
con permiso para publicar `claude-proxy-service` y `claude-proxy-cli` en crates.io.
La cuenta debe tener un correo verificado y ser propietaria de esos nombres,
o poder registrarlos en la primera publicación. No se publica el servidor en
crates.io; `claude-proxy-service` es la biblioteca necesaria para instalar el CLI.
El workflow usa `GITHUB_TOKEN` con `contents: write` para la release.

## Publicar una versión

1. Actualiza `workspace.package.version` en `Cargo.toml` y la versión exacta de
   `claude-proxy-service` en `crates/cli/Cargo.toml` (por ejemplo `=0.1.1`).
2. Actualiza el lockfile con `cargo check --workspace` y sube los cambios.
3. Crea y sube el tag correspondiente, por ejemplo `v0.1.1`.

```sh
git tag v0.1.1
git push origin v0.1.1
```

También puedes ejecutar **Release CLI + server** manualmente indicando un tag
existente. El workflow siempre utiliza el código del tag solicitado y verifica
que la versión coincida con todos los paquetes.

Primero ejecuta los tests y compila ambos binarios en las seis plataformas.
Después reúne y verifica todos los archivos y sus checksums, crea una sola
release en borrador, publica la biblioteca y después el CLI en crates.io y,
finalmente, hace pública la release completa. Un fallo en cualquier build
impide iniciar la publicación.

GitHub y crates.io no ofrecen una transacción conjunta: si falla crates.io,
puede haber paquetes ya publicados y la release queda en borrador; durante la
publicación inicial del CLI puede existir un breve intervalo antes de que el
servidor sea accesible públicamente. Reejecuta el mismo workflow para el mismo
tag: omite versiones ya publicadas en crates.io y completa la release existente.
No reutilices un tag para código distinto; crates.io no permite sobrescribir una
versión. Las releases ya públicas se verifican y sus archivos no se reemplazan.

Para comprobar los scripts sin publicar:

```sh
python -m unittest discover -s scripts -p 'test_release.py'
python scripts/verify_release.py --tag v0.1.0
```
