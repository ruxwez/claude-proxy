<script>
  import Modal from './Modal.svelte';
  import { app, persist, navigate, discover } from './state.svelte.js';
  import { presets } from './catalog.js';
  import { ArrowRight, Check } from '@lucide/svelte';
  let { open = $bindable(false), provider = null } = $props();
  let draft = $state(null);
  let error = $state('');
  let saving = $state(false);
  $effect(() => {
    if (open) {
      draft = provider ? structuredClone($state.snapshot(provider)) : null;
      error = '';
    }
  });
  function choose(kind) {
    let id = kind.replaceAll('_', '-'), n = 2;
    while (app.settings.providers.some(p => p.id === id)) id = `${kind.replaceAll('_', '-')}-${n++}`;
    draft = { id, kind, name: presets[kind].name, base_url: presets[kind].base, models_dev_provider: null, enabled: true, api_key: null, models: [] };
  }
  async function save(event) {
    event.preventDefault();
    saving = true; error = '';
    const previous = structuredClone($state.snapshot(app.settings));
    const value = structuredClone($state.snapshot(draft));
    value.models_dev_provider ||= null;
    if (value.api_key === null || value.api_key === undefined || (value.api_key === '' && value.has_api_key)) value.api_key = null;
    const index = app.settings.providers.findIndex(p => p.id === provider?.id);
    if (index < 0) app.settings.providers.push(value); else app.settings.providers[index] = value;
    try {
      await persist();
      const saved = app.settings.providers.find(p => p.id === value.id);
      if (saved.api_key !== null) saved.has_api_key = !!saved.api_key;
      saved.api_key = null;
      delete app.catalogs[value.id];
      discover(saved);
      open = false;
      navigate(`/providers/${value.id}`);
    } catch (failure) { app.settings = previous; error = failure.message; }
    finally { saving = false; }
  }
</script>

<Modal bind:open title={provider ? 'Configurar proveedor' : 'Añadir proveedor'} description={provider ? 'Credenciales, conexión y catálogo de modelos.' : 'Elige cómo quieres conectar tus modelos.'}>
  {#if !draft}
    <div class="space-y-2">
      {#each Object.entries(presets) as [kind, preset]}
        <button class="group flex w-full items-center gap-3 rounded-xl border border-line p-3 text-left transition-colors hover:border-zinc-500 hover:bg-elevated" onclick={() => choose(kind)}>
          <span class="flex size-10 shrink-0 items-center justify-center rounded-lg text-xs font-bold" style={`color:${preset.color};background:${preset.color}12`}>{preset.mark}</span>
          <span class="flex-1"><span class="block text-sm font-medium">{preset.name}</span><span class="text-xs text-muted">{preset.description}</span></span>
          <ArrowRight size={15} class="text-muted group-hover:text-accent" />
        </button>
      {/each}
    </div>
  {:else}
    <form onsubmit={save} class="space-y-4">
      <div class="flex items-center gap-2 text-xs text-muted"><span class="badge">{presets[draft.kind].name}</span>{#if !provider}<button type="button" class="ml-auto hover:text-white" onclick={() => draft = null}>Cambiar tipo</button>{/if}</div>
      <div class="grid grid-cols-2 gap-3">
        <label>Nombre<input required bind:value={draft.name} /></label>
        <label>Identificador<input required pattern="[A-Za-z0-9_-]+" bind:value={draft.id} disabled={!!provider} /></label>
      </div>
      {#if draft.kind !== 'chatgpt'}
        <label>URL base<input required type="url" bind:value={draft.base_url} /></label>
        <label>API key<input type="password" autocomplete="new-password" bind:value={draft.api_key} placeholder={draft.has_api_key ? 'Clave guardada · vacío para conservar' : 'Introduce tu API key'} /></label>
        {#if draft.has_api_key}<button type="button" class="text-xs text-red-300 hover:text-red-200" onclick={() => { draft.api_key = ''; draft.has_api_key = false; }}>Eliminar clave guardada al guardar</button>{/if}
      {:else}
        <div class="rounded-xl border border-line bg-canvas p-3 text-xs leading-relaxed text-muted">{draft.subscription_connected ? `Cuenta conectada: ${draft.subscription_account || 'ChatGPT'}` : 'Guarda el proveedor y conecta tu cuenta desde su página. ChatGPT utiliza exclusivamente la suscripción.'}</div>
      {/if}
      <label>Proveedor en models.dev<input list="metadata-providers" bind:value={draft.models_dev_provider} placeholder={draft.kind === 'openai_compatible' ? 'Identificador de models.dev' : 'Automático'} /></label>
      <datalist id="metadata-providers">{#each app.metadataProviders as p}<option value={p.id}>{p.name}</option>{/each}</datalist>
      <label class="flex-row items-center"><input type="checkbox" bind:checked={draft.enabled} /> Proveedor activo</label>
      {#if error}<p role="alert" class="text-xs text-red-300">{error}</p>{/if}
      <div class="flex justify-end gap-2 border-t border-line pt-4"><button type="button" class="btn" onclick={() => open = false} disabled={saving}>Cancelar</button><button class="btn-primary" disabled={saving}><Check size={14} />{saving ? 'Guardando…' : provider ? 'Guardar cambios' : 'Añadir proveedor'}</button></div>
    </form>
  {/if}
</Modal>
