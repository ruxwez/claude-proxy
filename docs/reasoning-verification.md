# Verificación de razonamiento y multiagente

Fecha: 11 de octubre de 2026. Claude Code: **2.1.296**. Proveedor real:
OpenCode Go, usando la configuración local de `.env` y un proxy aislado por
ejecución. `provider.rs` queda fuera de estos cambios.

## Resultados reales

| Modelo público | Comprobación | Resultado |
|---|---|---|
| `opencode-go/gpt-6-luna` | Responses JSON: razonamiento → herramienta → resultado → respuesta | Correcto |
| `opencode-go/gpt-6-luna` | Responses SSE: mismo recorrido | Correcto |
| `opencode-go/gpt-5.6-luna` | Responses SSE: mismo recorrido | Correcto |
| `opencode-go/deepseek-v4.1-flash` | Chat JSON: razonamiento → herramienta → resultado → respuesta | Correcto |
| `opencode-go/deepseek-v4.1-flash` | Chat SSE: mismo recorrido | Correcto |
| `opencode-go/gpt-6-luna` | Claude Code: dos subagentes mediante `Agent` | Correcto |
| `opencode-go/deepseek-v4.1-flash` | Claude Code: dos subagentes mediante `Agent` | Correcto |
| `opencode-go/gpt-6-luna` | Claude Code: `Workflow` con dos subagentes en paralelo | Correcto |
| `opencode-go/deepseek-v4.1-flash` | Claude Code: `Workflow` con dos subagentes en paralelo | Correcto |

En los recorridos de herramientas se verificó mediante SHA-256 que el estado
reenviado coincide exactamente con un estado recibido previamente. Las
continuaciones devolvieron `SMOKE_OK`, con HTTP 200 en ambos turnos. Responses
puede entregar ciphertext distinto en `output_item.done` y `response.completed`;
se conserva una sola copia por ID y la continuación real aceptó esa copia.

Los casos `Agent` registraron dos llamadas reales, mensajes de los hijos y dos
notificaciones de finalización, antes de `SUBAGENTS_OK ALPHA_42 BETA_43`.
Los casos `Workflow` registraron una llamada real con un script que usa
paralelismo, dos peticiones de hijos al mismo modelo, eventos de progreso y
finalización del runtime, y `WORKFLOW_OK ALPHA_42 BETA_43`. Se comprobó también
el reenvío de estado durante las continuaciones del workflow.

El [resumen de evidencia](reasoning-verification.json) contiene conteos y
comprobaciones de igualdad, sin claves, prompts, ciphertext ni pensamiento.
Los informes detallados de la ejecución local están en `target/verification/`.

## Cómo repetirlo

El script [verify_reasoning.py](../scripts/verify_reasoning.py) requiere Python,
el ejecutable de depuración recién compilado y las credenciales de `.env`.
Los modos `subagents` y `workflow` requieren Claude Code disponible en `PATH`.
Ejemplo:

```sh
cargo build -p claude-proxy
python scripts/verify_reasoning.py --mode roundtrip --model opencode-go/gpt-6-luna --report target/verification/responses-json.json
python scripts/verify_reasoning.py --mode roundtrip --model opencode-go/gpt-6-luna --stream --report target/verification/responses-stream.json
python scripts/verify_reasoning.py --mode subagents --model opencode-go/deepseek-v4.1-flash --report target/verification/subagents.json
python scripts/verify_reasoning.py --mode workflow --model opencode-go/deepseek-v4.1-flash --report target/verification/workflow.json
```

Se arranca un proxy en un puerto efímero, con un relay que observa únicamente la
estructura y compara hashes en memoria. Claude se ejecuta en una carpeta temporal,
sin hooks ni configuración del proyecto, con las herramientas necesarias
permitidas y un máximo de seis turnos. Las peticiones reales consumen cuota.

## Alcance de la comprobación

Los workflows se solicitaron explícitamente con `Workflow` permitido, siguiendo
el [procedimiento documentado por Anthropic](https://platform.claude.com/cookbook/claude-agent-sdk-08-dynamic-workflows).
Esto verifica su ejecución real con modelos no Anthropic a través del proxy;
no verifica que el cliente decida activarlos automáticamente ni la selección
interactiva de Ultracode.

El relay reúne la respuesta SSE antes de reenviarla: estos smoke checks verifican
el contenido y el ciclo completo del cliente. La entrega fragmentada, UTF-8
dividido, argumentos intercalados, timeouts y cierres truncados se verifican en
los tests HTTP y unitarios. La validación local pasó **37 pruebas** (27 del proxy
y 10 del sincronizador), formato y Clippy con warnings tratados como errores.

La conservación del estado depende de que el cliente mantenga los bloques del
proxy en su historial. No se garantiza compatibilidad universal con modelos,
clientes, proveedores ni workflows que no figuran en estas comprobaciones.
