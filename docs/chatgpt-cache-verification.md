# Comprobación de caché ChatGPT — 2026-10-13

Se ejecutaron dos peticiones consecutivas idénticas a través de una instancia
aislada del proxy, utilizando la sesión ChatGPT conectada y `gpt-6-luna`.
La base y configuración del servicio instalado no se modificaron. Las copias
temporales de credenciales se eliminaron al terminar.

La entrada contenía un prefijo sintético de 125 entradas, un marcador Anthropic
`cache_control` en el sistema y la instrucción de responder únicamente `OK`.
Se utilizó SSE y esfuerzo `low`, sin herramientas. Se solicitó un máximo de 32
tokens; la ruta de suscripción no documenta soporte para `max_output_tokens`,
por lo que ese parámetro no debe considerarse una garantía de límite remoto.

| Llamada | HTTP | Entrada | Salida | Razonamiento | Lecturas de caché | Escrituras de caché |
|---|---|---|---|---|---|---|
| 1 | 200 | 1652 | 5 | 0 | 0 | 0 |
| 2 | 200 | 1652 | 5 | 0 | 0 | 0 |

Ambas finalizaron con `message_delta` válido. Los contadores proceden del uso
comunicado por el proveedor y traducido por el proxy. Apareció el aviso
`prompt_cache_uses_provider_defaults`: el adaptador elimina los controles
explícitos de caché antes de enviar la petición de suscripción.

Esta prueba no obtuvo reutilización ni escritura de caché. No demuestra que
todas las peticiones carezcan de caché, ni atribuye el agotamiento de cuota de
otro servidor: no se dispone de sus trazas, modelo, volumen ni consumo previo.
Tampoco se encontró un bucle de reintentos de inferencia en el recorrido ChatGPT.

El historial del repositorio documenta rechazos HTTP 400 de los controles
explícitos de caché el 2026-10-12.

Tras autorizar el usuario hasta otras dos llamadas, se compiló una variante
experimental que conserva `prompt_cache_options` y `prompt_cache_breakpoint`.
La primera petición, idéntica a la anterior, devolvió HTTP 400 con el mensaje
`prompt_cache_options is not supported on this model`. Se detuvo la prueba y no
se ejecutó la segunda llamada. No se recibieron contadores de uso en el rechazo.
La variante experimental se retiró; se conserva la adaptación existente.

Por tanto, el servicio real rechaza los controles explícitos para este modelo
y esta suscripción. Su eliminación evita ese error y no basta para explicar
los ceros de caché observados.

## Causa y corrección verificadas

El cliente oficial de Codex documenta que ChatGPT deriva su afinidad de caché
de la cabecera `session-id`, y envía la misma identidad en `prompt_cache_key`.
El proxy no enviaba ninguno de los dos campos para ChatGPT.

Se añadió una clave determinista por proveedor, modelo y conversación, utilizada
tanto en el cuerpo Responses como en la cabecera. Se obtiene de las cabeceras
de sesión del cliente, de `metadata.user_id` en formato JSON/legado Claude Code,
o del primer mensaje como alternativa sin estado. Se envía un SHA-256; no se
expone el identificador ni el texto original. Añadir turnos conserva la clave.

Primero se repitió la prueba corta con la corrección: ambas llamadas de 1652
tokens siguieron reportando cero caché. Se amplió únicamente el prefijo sintético
a 225 entradas (2952 tokens) y se obtuvo reutilización. Después se compiló el
código sin la corrección de afinidad y se repitió exactamente el mismo prefijo
largo: no hubo aciertos. Ambas variantes utilizan las mismas credenciales,
modelo, instrucciones, esfuerzo y adaptación de controles explícitos.

| Variante | Llamada | Entrada total | Entrada sin caché | Caché leída | Salida | Razonamiento |
|---|---|---|---|---|---|---|
| Afinidad estable | 1 | 2952 | 2952 | 0 | 5 | 0 |
| Afinidad estable | 2 | 2952 | 136 | 2816 | 5 | 0 |
| Sin afinidad (control posterior) | 1 | 2952 | 2952 | 0 | 5 | 0 |
| Sin afinidad (control posterior) | 2 | 2952 | 2952 | 0 | 5 | 0 |

Las cuatro respuestas fueron HTTP 200 con final válido. La segunda llamada
corregida reutilizó el 95,39 % de la entrada. `cache_write_tokens` siguió siendo
cero incluso cuando se obtuvo un acierto: este campo no demuestra por sí solo
la ausencia de escritura en la suscripción. El resultado corto tampoco permite
determinar el umbral exacto efectivo de esta ruta.

El control confirma la mejora para este caso. No implica aciertos garantizados
para todo tráfico ni prueba cuánto consumió el incidente del otro servidor.
En toda la investigación se completaron ocho inferencias pequeñas (40 tokens
de salida en total, cero de razonamiento) y un rechazo HTTP 400. No se ejecutaron
agentes ni herramientas del modelo.

La corrección conserva la omisión de controles incompatibles. Las regresiones
HTTP comprueban estabilidad al añadir turnos, separación entre conversaciones,
identidades Claude Code en cabeceras/JSON/legado, alternativa sin sesión,
privacidad del identificador y ausencia de cambios en proveedores con API key.

La comprobación es reproducible, con autorización para consumir cuota:

```sh
cargo build -p claude-proxy-server --locked
python scripts/verify_chatgpt_cache.py --database /ruta/proxy.sqlite3
```

El script limita la ejecución a dos peticiones GPT-6 Luna, sin reintentos, con
esfuerzo `low`, y detiene el segundo intento si el primero falla.

Se encontró y corrigió por separado la lectura del catálogo: la suscripción
devuelve `models`, `slug` y `visibility`, mientras el adaptador solo admitía
el formato de API key `data` e `id`. Ahora convierte los modelos visibles al
contrato del proxy y descarta instrucciones y ajustes internos del proveedor.
Este cambio está en el código fuente; no se sustituyó el servicio instalado.
El endpoint de catálogo corregido se comprobó con la sesión real desde una
instancia aislada: devolvió siete modelos visibles y ningún aviso de error,
sin ejecutar inferencias. Validación final: 59 pruebas del servidor correctas,
una prueba de bases remotas omitida, formato y Clippy sin errores.

Referencias oficiales consultadas:

- [Modelos e inferencia de ChatGPT](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference).
- [Limitaciones de la suscripción](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).
- [Caché de prompts de OpenAI](https://developers.openai.com/api/docs/guides/prompt-caching).
- [Implementación oficial de afinidad ChatGPT en Codex](https://github.com/openai/codex/commit/bc5957eac9e89e66f990ed490d11e625a4a3b02c).
