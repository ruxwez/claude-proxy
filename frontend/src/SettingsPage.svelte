<script>
  import { Check, RotateCcw, Terminal, Copy, LoaderCircle } from '@lucide/svelte';
  import { app, persist } from './state.svelte.js';
  let draft = $state({ ...structuredClone($state.snapshot(app.settings)), default_model: app.settings.default_model || '' });
  let saving = $state(false);
  let error = $state('');
  let copied = $state(false);
  let models = $derived(app.settings.providers.filter(p => p.enabled).flatMap(p => p.models.filter(m => m.enabled).map(m => ({ id: `${p.id}/${m.id}`, name: `${p.name} / ${m.display_name || m.id}` }))));
  async function save(event) {
    event.preventDefault(); saving = true; error = '';
    const previous = { context_tokens: app.settings.context_tokens, compact_tokens: app.settings.compact_tokens, timeout_seconds: app.settings.timeout_seconds, public_url: app.settings.public_url, default_model: app.settings.default_model };
    Object.assign(app.settings, { context_tokens: draft.context_tokens || null, compact_tokens: draft.compact_tokens || null, timeout_seconds: draft.timeout_seconds, public_url: draft.public_url || null, default_model: draft.default_model || null });
    try { await persist(); } catch (failure) { Object.assign(app.settings, previous); error = failure.message; }
    finally { saving = false; }
  }
</script>

<div class="mb-7"><p class="eyebrow mb-2">Configuración</p><h1 class="text-2xl font-semibold tracking-tight">Preferencias</h1><p class="mt-2 text-xs text-muted">El comportamiento del proxy y la configuración que recibe Claude Code.</p></div>
<form onsubmit={save} class="max-w-3xl space-y-5">
  <section class="rounded-xl border border-line bg-panel p-5"><div class="mb-5 flex items-center justify-between"><div><h2 class="font-semibold">Contexto y compactación</h2><p class="mt-1 text-xs text-muted">Los valores originales siguen siendo 990K / 900K.</p></div><button type="button" class="btn !py-1.5" onclick={() => { draft.context_tokens = 990000; draft.compact_tokens = 900000; }}><RotateCcw size={13} /> Restaurar</button></div><div class="grid gap-4 sm:grid-cols-2"><label>Ventana de contexto<input type="number" min="1" bind:value={draft.context_tokens} placeholder="Predeterminado de Claude" /></label><label>Compactación automática<input type="number" min="1" bind:value={draft.compact_tokens} placeholder="Predeterminada de Claude" /></label></div><p class="mt-3 text-[11px] leading-relaxed text-muted">Estos límites son globales. Ajusta los valores a tus modelos; dejar un campo vacío utiliza el comportamiento de Claude Code.</p></section>
  <section class="rounded-xl border border-line bg-panel p-5"><h2 class="mb-5 font-semibold">Claude Code</h2><div class="space-y-4"><label>Modelo predeterminado<select bind:value={draft.default_model}><option value="">Conservar el modelo actual si está disponible</option>{#each models as model}<option value={model.id}>{model.name}</option>{/each}</select></label><label>URL pública de Anthropic<input type="url" bind:value={draft.public_url} placeholder="Automática: mismo host, puerto 10800" /></label><label>Timeout del proveedor (segundos)<input class="sm:max-w-48" type="number" min="1" max="3600" required bind:value={draft.timeout_seconds} /></label></div></section>
  {#if error}<p role="alert" class="text-xs text-red-300">{error}</p>{/if}
  <div class="flex justify-end"><button class="btn-primary" disabled={saving}>{#if saving}<LoaderCircle size={14} class="animate-spin" />{:else}<Check size={14} />{/if} Guardar preferencias</button></div>
</form>
<section class="mt-8 max-w-3xl rounded-xl border border-line bg-panel/40 p-5"><div class="mb-2 flex items-center gap-2 font-medium"><Terminal size={16} class="text-accent" /> Sincronizar con Claude Code</div><p class="mb-4 text-xs text-muted">Ejecuta este comando después de cambiar tus modelos o preferencias y reinicia Claude Code.</p><div class="flex items-center justify-between gap-3 rounded-lg border border-line bg-canvas p-3"><code class="break-all text-accent">claude-update-models --endpoint {window.location.origin}</code><button type="button" class="icon-btn" aria-label="Copiar comando" onclick={async () => { try { await navigator.clipboard.writeText(`claude-update-models --endpoint ${window.location.origin}`); copied = true; } catch { error = 'No se pudo copiar. Selecciona el comando y cópialo manualmente.'; } }}>{#if copied}<Check size={15} />{:else}<Copy size={15} />{/if}</button></div></section>
