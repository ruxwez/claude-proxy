# Compatibilidad de protocolos

Implementación verificada el 12 de octubre de 2026. La entrada es Anthropic
Messages; cada modelo elige Messages, Chat Completions o Responses. No existe
equivalencia universal entre proveedores ni se presupone soporte del modelo
por aceptar un formato de transporte.

| Función | Messages | Chat Completions | Responses |
|---|---|---|---|
| Texto e instrucciones | Nativo | Se conserva | Se conserva |
| Imágenes URL/base64 | Nativo | `image_url` | `input_image` |
| PDF base64 | Nativo | Parte `file` | Parte `input_file` |
| Documento por URL | Nativo | Error: elegir Responses | `input_file.file_url` |
| Documento de texto/contenido | Nativo | Texto/partes con título y contexto | Texto/partes con título y contexto |
| `file_id` de Anthropic | Nativo | Error: enviar contenido | Error: enviar contenido |
| Herramientas y resultados paralelos | Nativo | Funciones y mensajes `tool` | `function_call`/`function_call_output` |
| Resultados con imágenes/PDF | Nativo | Mensaje de usuario asociado después de los resultados | Contenido multimedia en el resultado |
| `tool_result.is_error` | Nativo | Marca explícita en el resultado | Marca explícita en el resultado |
| Herramientas `strict` | Nativo | Se conserva | Se conserva; ausencia significa `false` |
| Nombres largos o incompatibles | Nativo | Alias determinista y restauración al responder | Alias determinista y restauración al responder |
| Ejemplos de argumentos | Nativo | Se incorporan a la descripción | Se incorporan a la descripción |
| Herramientas diferidas/referencias | Nativo | Carga inmediata; conserva descubrimiento | Carga inmediata; conserva descubrimiento |
| JSON Schema de salida | Nativo | `response_format.json_schema` | `text.format` |
| Rechazos/filtros | Nativo | Texto y `stop_reason: refusal` | Texto y `stop_reason: refusal` |
| Citas/anotaciones | Nativo | Referencias legibles con metadatos originales | Referencias legibles con metadatos originales |
| Secuencias de parada | Nativo | Proveedor y filtro local | Filtro local |
| Totales de tokens | Nativo | Se traducen | Se traducen |
| Caché de entrada | Nativo | `cache_read_input_tokens` si se informa | `cache_read_input_tokens` si se informa |
| Detalle de razonamiento/uso | Nativo | Extensiones de `usage` | Extensiones de `usage` |
| Conteo previo de tokens | Endpoint nativo | Estimación identificada | Endpoint `responses/input_tokens` |
| Betas de Anthropic | Se reenvían | Aviso: no transferibles | Aviso: no transferibles |
| Cambios de esfuerzo dentro del historial | Nativo | Error: elegir Responses | `configuration_update` en su posición |

Los PDFs e imágenes requieren un modelo capaz de interpretarlos. El proxy no
descarga documentos ni intenta reutilizar identificadores de archivo entre
servicios. En Chat, la asociación de medios con una herramienta se representa
mediante texto explícito en un mensaje de usuario: conserva el contenido, pero
no es un resultado multimedia nativo.

## Razonamiento

Responses conserva los items completos de razonamiento cifrado. Chat conserva
`reasoning_content`, `reasoning` y los objetos de `reasoning_details`, incluidas
sus firmas. Los fragmentos de `reasoning_details` se concatenan en el orden
recibido, conforme al contrato de OpenRouter. DeepSeek mantiene su tratamiento
específico de contenido y pensamiento.

Responses también conserva la fase, el ID, el estado y las anotaciones de los
mensajes originales mediante el mismo transporte del historial. Al continuar,
restaura el mensaje original cuando su texto visible sigue coincidiendo; si el
cliente edita o trunca ese texto se utiliza el contenido editado y no se repone
una versión antigua. Esto conserva la distinción entre comentario y respuesta
final sin duplicar el texto que ve Claude Code.

Se transportan en bloques `redacted_thinking` propios del proxy y se restituyen
al mismo identificador público de modelo/protocolo. No son firmas de Anthropic.
Si el cliente los elimina o compacta, o se cambia de modelo, no puede garantizarse
la continuidad del estado. El pensamiento nativo de otro servicio no se convierte
en una firma equivalente. Los controles de esfuerzo siguen las políticas de IDs
reconocidos descritas en el README; los alias desconocidos producen un aviso si
se solicitan controles de pensamiento.

`budget_tokens` no tiene equivalencia exacta con un nivel de esfuerzo. La
temperatura y `top_p` pueden omitirse cuando el modelo de razonamiento no los
admite. `ultra` es una opción de orquestación del cliente, no un esfuerzo de API
que pueda reproducirse mediante una sola petición.

## Caché, uso y parada

En modelos OpenAI reconocidos GPT-5.6 y posteriores se traducen los puntos de
caché de partes compatibles a `prompt_cache_breakpoint`. Cuando no hay una
frontera representable, se usa la política implícita del proveedor. El TTL de
Anthropic de 5 minutos/1 hora no equivale al de OpenAI de 30 minutos: se informa
la diferencia, sin prometer igual retención, facturación o aciertos. En otros
modelos no se envían campos de caché OpenAI específicos.

ChatGPT por suscripción usa la caché automática del proveedor: se omiten
`prompt_cache_options` y los marcadores `prompt_cache_breakpoint`, porque su
servicio puede rechazarlos incluso con esos IDs de modelo. El aviso
`prompt_cache_uses_provider_defaults` identifica la adaptación; no se eliminan
texto, herramientas ni contadores de caché recibidos. OpenAI con API key conserva
la conversión anterior. También se aplica a las entradas de `count_tokens`.
Para mantener la afinidad entre turnos se envía una clave determinista por
proveedor, modelo y conversación en `prompt_cache_key` y `session-id`. Las
identidades del cliente se convierten a SHA-256. Si no hay sesión identificable,
se utiliza el primer mensaje como alternativa estable. El cambio se verificó
con GPT-6 Luna: 2816 de 2952 tokens reutilizados en la segunda llamada, frente
a cero en el control sin afinidad. Consulte el
[informe del 2026-10-13](chatgpt-cache-verification.md); los aciertos y la cuota
efectiva siguen dependiendo del proveedor.
El 2026-10-12 se reprodujo el 400 anterior y se verificó la corrección en vivo:
`gpt-6.1-sol`, `gpt-6-astra` y `gpt-6-luna` devolvieron HTTP 200 y `OK`, con fin
de streaming sin errores. Las peticiones incluyeron marcadores de caché en
sistema, usuario, herramientas y resultado de herramienta, además de
`metadata.user_id`. Pasaron 57 pruebas del proxy y 9 del CLI, formato y Clippy.

El `input_tokens` de Anthropic excluye los tokens cacheados. El proxy resta los
aciertos comunicados por el proveedor y los coloca en `cache_read_input_tokens`
para evitar contar dos veces. Los campos `input_tokens_details` y
`output_tokens_details` de `usage` son extensiones: conservan detalles originales
que algunos clientes pueden ignorar. No se inventan cifras de escritura de
caché que el proveedor no entregue.

El filtro de parada retiene posibles prefijos entre fragmentos UTF-8 y devuelve
`stop_reason: stop_sequence` con la secuencia correspondiente. No transmite
texto ni nuevas herramientas posteriores a la coincidencia. Se sigue leyendo
el stream para obtener el consumo real: la parada local no evita que el proveedor
genere o facture esos tokens.

`x-claude-proxy-token-count: provider` indica un conteo real. `estimated` indica
una aproximación, utilizada en Chat, cuando falta el modelo o cuando el endpoint
del proveedor responde 404/405/501. Un 401, 429 u otro rechazo no se oculta con
una estimación. Esta aproximación sigue sin ser un tokenizador específico del
modelo y puede afectar a la compactación de Claude Code.

## Funciones sin equivalente

Las herramientas exclusivas del servidor, MCP ejecutado por el proveedor,
contenedores, edición de contexto y formatos de contenido desconocidos devuelven
un error explícito en las conversiones. `top_k`, residencia de inferencia y
velocidad nativa tampoco se transfieren. Los campos superiores desconocidos se
rechazan, mientras Messages los conserva para que los valide su proveedor.

Las anotaciones se convierten en texto con los datos originales; no se fabrican
objetos de citas nativas ni índices cifrados. Los argumentos de herramientas en
SSE se validan y entregan cuando la llamada está completa, para conservar sus
metadatos y evitar ejecutar JSON parcial. Cambia la latencia de entrega respecto
al streaming fino nativo.

`metadata.user_id` se reenvía como hash estable (`user` o `safety_identifier`),
con aviso. ChatGPT por suscripción omite `safety_identifier`, rechazado por su
servicio, y devuelve el aviso `user_id_not_supported_by_subscription`; OpenAI
con API key conserva el hash. La identidad de autenticación no se modifica.
Verificado el 2026-10-12 con una petición real a `proveedor-1/gpt-6.1-sol`,
incluyendo `metadata.user_id`: HTTP 200, texto `OK` y fin de streaming sin
errores. La regresión comprueba JSON/SSE y que API key sigue enviando el hash.
`service_tier: auto/standard_only` se convierte a `auto/default`;
no garantiza equivalencia de las condiciones comerciales del proveedor.

## Diagnóstico y verificación

`x-claude-proxy-warnings` contiene códigos de adaptaciones de la petición,
también registrados sin prompts ni credenciales. No se insertan esos avisos en
las instrucciones del modelo. Claude Code puede ignorar las cabeceras, por lo
que la cabecera y el log son el lugar para diagnosticar estas diferencias.

Se conservan identificadores de petición, límites de cuota y `retry-after` del
proveedor. No se reenvían cookies. Messages conserva cuerpos de error y eventos
de error nativos; los otros protocolos traducen los tipos de error según su
estado HTTP. Streams incompletos y contenido de salida no representable producen
errores explícitos.

Esto incluye todas las cabeceras `anthropic-ratelimit-unified-*`, sin modificar
sus valores, tanto en mensajes JSON/SSE como en respuestas de `count_tokens`
(éxito, error o endpoint ausente). Se conservan también campos futuros de esa
familia. No se fabrican cabeceras unified a partir de límites de peticiones por
minuto: esos límites no equivalen a la cuota de una suscripción. Si el proveedor
no proporciona esa cuota, los avisos de uso del cliente no están garantizados.

`count_tokens` consulta `/messages/count_tokens` en Messages y
`/responses/input_tokens` en Responses; devuelve el conteo del proveedor con
`x-claude-proxy-token-count: provider`. Chat Completions y endpoints ausentes
usan la aproximación local identificada como `estimated`; no se lanza una
inferencia facturable para obtener un conteo. Los errores nativos de conteo
Messages conservan su cuerpo y cabeceras, incluidos los límites unified.

Las pruebas en `protocol_tests.rs` cubren contenido multimedia, resultados
paralelos, errores, documentos, esquemas estrictos, referencias, rechazos,
reconciliación de eventos finales, razonamiento entre turnos, nombres de
herramientas, caché, parada y los endpoints HTTP de conteo. Las pruebas reales
históricas están en [reasoning-verification.md](reasoning-verification.md).
La disponibilidad de ChatGPT por suscripción y de cada modelo requiere una
validación independiente de autenticación y permisos.

Comprobaciones reales de esta implementación, con una copia aislada de SQLite:

| Modelo | Recorrido | Resultado |
|---|---|---|
| `opencode-go/deepseek-v4.1-flash` | JSON: razonamiento → herramienta → resultado → continuación | Correcto |
| `opencode-go/deepseek-v4.1-flash` | SSE: mismo recorrido | Correcto |
| `opencode-go/muse-spark-1.3-contributor` | SSE Responses: estado cifrado → herramienta → resultado → continuación | Correcto |

Se compararon hashes del estado recibido y reenviado, sin guardar pensamiento ni
credenciales en los informes. [Evidencia sanitizada](protocol-verification.json).
Pasaron 54 pruebas del proxy y 9 del CLI, formato y Clippy. El contenedor Docker
se reconstruyó y arrancó saludable, verificando que la configuración pública
antes y después del reinicio era idéntica. Las comprobaciones de imágenes, PDFs,
esquemas y rechazos usan proveedores simulados; no acreditan que todos los
modelos disponibles admitan esas capacidades.

Referencias: [resultados de herramientas Anthropic](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls),
[funciones OpenAI](https://developers.openai.com/api/docs/guides/function-calling),
[archivos OpenAI](https://developers.openai.com/api/docs/guides/file-inputs),
[caché OpenAI](https://developers.openai.com/api/docs/guides/prompt-caching),
[conteo OpenAI](https://developers.openai.com/api/docs/guides/token-counting),
[razonamiento OpenRouter](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens).
