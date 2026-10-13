# Pruebas automáticas

El workflow **Tests** (`.github/workflows/ci.yml`) se ejecuta con cada push a una
rama, en las pull requests y manualmente desde GitHub Actions. Es independiente
del workflow de publicación y no requiere configurar secretos ni API keys de
OpenAI, Anthropic, Zed u otros proveedores.

- En Linux, Windows y macOS comprueba formato y Clippy, ejecuta los tests Rust
  y los tests Python del empaquetado, compila CLI y servidor y verifica su
  integración y la instalación del servicio nativo. La prueba interactiva de
  terminal se ejecuta en Linux y macOS.
- PostgreSQL y MySQL se crean como contenedores temporales del propio job,
  usando credenciales ficticias, para ejecutar la prueba de persistencia que
  `cargo test` omite normalmente.
- Otro job compila Docker y comprueba los endpoints de salud y administración.

Las pruebas de proveedores levantan servidores HTTP locales y usan claves de
prueba. No hacen inferencias de pago. Sí necesitan acceso a Internet para
descargar herramientas, dependencias de Cargo, acciones e imágenes Docker.
Las pruebas de servicio instalan servicios reales: se ejecutan en los runners
temporales de GitHub, no deben lanzarse en una máquina que ya use ese servicio.

Para ejecutar las pruebas Rust en local:

```sh
cargo test --workspace --locked
```

`scripts/verify_reasoning.py` es una comprobación opcional con proveedores reales:
usa las credenciales configuradas en la base de datos y puede consumir cuota.
No forma parte del workflow **Tests**.

`scripts/verify_chatgpt_cache.py --database /ruta/proxy.sqlite3` realiza dos
peticiones pequeñas con GPT-6 Luna y esfuerzo bajo para comprobar la caché
de una sesión ChatGPT conectada. Consume cuota y solo debe ejecutarse con
autorización. Utiliza configuración aislada, no modifica el servicio y no
imprime credenciales. El informe está en
[chatgpt-cache-verification.md](chatgpt-cache-verification.md).
