<script>
  import { onMount } from 'svelte';
  import { DropdownMenu } from 'bits-ui';
  import { Search, RotateCw, SlidersHorizontal, Plus, MoreHorizontal, Brain, Wrench, Image, Check, ArrowLeft, ExternalLink, Trash2, Power, LoaderCircle, Info, LogIn, LogOut } from '@lucide/svelte';
  import Modal from './Modal.svelte';
  import ProviderDialog from './ProviderDialog.svelte';
  import { app, api, persist, navigate, discover } from './state.svelte.js';
  import { presets, modelRows, selectModel, protocol, credential, tokens, number, price } from './catalog.js';
  let { provider } = $props();
  let search = $state('');
  let filter = $state('all');
  let editing = $state(false);
  let modelOpen = $state(false);
  let detailsOpen = $state(false);
  let deleting = $state(false);
  let modelDraft = $state(null);
  let detailId = $state(null);
  let localError = $state('');
  let busy = $state(false);
  let catalog = $derived(app.catalogs[provider.id]);
  let rows = $derived(modelRows(provider, catalog?.data));
  let detail = $derived(rows.find(row => row.id === detailId));
  let visible = $derived(rows.filter(m => `${m.id} ${m.display_name}`.toLowerCase().includes(search.toLowerCase()) && (filter !== 'active' || m.selected?.enabled)));
  let activeCount = $derived(provider.models.filter(m => m.enabled).length);
  let allActive = $derived(visible.length > 0 && visible.every(m => m.selected?.enabled));
  onMount(() => { if (!app.catalogs[provider.id]) discover(provider); });
  function saveSelection() {
    if (app.settings.default_model && !app.settings.providers.some(p => p.enabled && p.models.some(m => m.enabled && `${p.id}/${m.id}` === app.settings.default_model))) app.settings.default_model = null;
    persist().catch(() => {});
  }
  function toggle(row, enabled) { selectModel(provider, row, enabled); saveSelection(); }
  function editModel(row = null) {
    modelDraft = row ? { ...(row.selected || { id: row.id, display_name: row.display_name, protocol: row.suggested_protocol || protocol(provider), credential: credential(provider), enabled: false }), original: row.id } : { id: '', display_name: '', protocol: protocol(provider), credential: credential(provider), enabled: true, original: null };
    modelOpen = true; localError = '';
  }
  async function saveModel(event) {
    event.preventDefault(); localError = '';
    if (!modelDraft.id || modelDraft.id.trim() !== modelDraft.id || /[\x00-\x1f\x7f]/.test(modelDraft.id)) { localError = 'Introduce un ID válido, sin espacios al principio o al final.'; return; }
    if (provider.models.some(m => m.id === modelDraft.id && m.id !== modelDraft.original)) { localError = 'Ya existe un modelo con este ID.'; return; }
    const value = { id: modelDraft.id, display_name: modelDraft.display_name, protocol: modelDraft.protocol, credential: credential(provider), enabled: modelDraft.enabled };
    const index = provider.models.findIndex(m => m.id === modelDraft.original);
    if (index < 0) provider.models.push(value); else provider.models[index] = value;
    saveSelection(); modelOpen = false;
  }
  async function connect() {
    busy = true; localError = '';
    try { await persist(); const result = await api(`/api/providers/${provider.id}/oauth`, 'POST', {}); window.location.assign(result.url); }
    catch (error) { localError = error.message; busy = false; }
  }
  async function disconnect() {
    busy = true; localError = '';
    try { const result = await api(`/api/providers/${provider.id}/oauth`, 'DELETE', {}); provider.subscription_connected = false; provider.subscription_account = null; delete app.catalogs[provider.id]; if (!result.remote_revoked) localError = 'Sesión desconectada localmente. Revisa el acceso autorizado en tu cuenta de ChatGPT.'; }
    catch (error) { localError = error.message; }
    finally { busy = false; }
  }
  async function remove() {
    busy = true;
    const previous = app.settings.providers;
    const previousDefault = app.settings.default_model;
    app.settings.providers = previous.filter(p => p.id !== provider.id);
    if (app.settings.default_model?.startsWith(`${provider.id}/`)) app.settings.default_model = null;
    try { await persist(); deleting = false; navigate('/providers'); }
    catch (error) { app.settings.providers = previous; app.settings.default_model = previousDefault; localError = error.message; }
    finally { busy = false; }
  }
</script>

<a href="/providers" onclick={e => { e.preventDefault(); navigate('/providers'); }} class="mb-5 inline-flex items-center gap-1.5 text-[11px] text-muted hover:text-white"><ArrowLeft size={13} /> Proveedores</a>
<div class="mb-6 flex flex-wrap items-start justify-between gap-4">
  <div class="flex items-center gap-3"><span class="flex size-12 items-center justify-center rounded-xl border border-line text-sm font-semibold" style={`background:${presets[provider.kind].color}08;color:${presets[provider.kind].color}`}>{presets[provider.kind].mark}</span><div><div class="flex items-center gap-3"><h1 class="text-2xl font-semibold tracking-tight">{provider.name}</h1><span class={`badge ${provider.enabled ? 'text-accent' : ''}`}><span class={`size-1 rounded-full ${provider.enabled ? 'bg-accent' : 'bg-zinc-500'}`}></span>{provider.enabled ? 'Activo' : 'Pausado'}</span></div><p class="mt-1 text-xs text-muted">{presets[provider.kind].description}</p></div></div>
  <div class="flex items-center gap-2"><button class="btn" onclick={() => editing = true}><SlidersHorizontal size={14} /> Configurar</button>
    <DropdownMenu.Root><DropdownMenu.Trigger class="icon-btn border border-line" aria-label="Acciones del proveedor"><MoreHorizontal size={17} /></DropdownMenu.Trigger><DropdownMenu.Portal><DropdownMenu.Content class="menu" sideOffset={8} align="end"><DropdownMenu.Item class="menu-item" onSelect={() => { provider.enabled = !provider.enabled; saveSelection(); }}><Power size={14} />{provider.enabled ? 'Pausar proveedor' : 'Activar proveedor'}</DropdownMenu.Item><DropdownMenu.Item class="menu-item text-red-300" onSelect={() => deleting = true}><Trash2 size={14} /> Eliminar proveedor</DropdownMenu.Item></DropdownMenu.Content></DropdownMenu.Portal></DropdownMenu.Root>
  </div>
</div>
<div class="mb-6 flex flex-wrap items-center gap-x-5 gap-y-2 rounded-xl border border-line bg-panel px-4 py-3 text-xs">
  <span class="flex items-center gap-2"><span class={`size-1.5 rounded-full ${provider.kind === 'chatgpt' ? provider.subscription_connected ? 'bg-accent' : 'bg-amber-300' : provider.has_api_key ? 'bg-accent' : 'bg-zinc-500'}`}></span>{provider.kind === 'chatgpt' ? provider.subscription_account || 'Cuenta sin conectar' : provider.has_api_key ? 'API key conectada' : 'Sin API key'}</span>
  <span class="hidden h-3 w-px bg-line sm:block"></span><span class="text-muted">{activeCount} activos <span class="mx-1 text-zinc-600">/</span> {rows.length} modelos</span>
  {#if provider.kind === 'chatgpt'}<button class="btn ml-auto !py-1.5" disabled={busy} onclick={provider.subscription_connected ? disconnect : connect}>{#if provider.subscription_connected}<LogOut size={13} /> Desconectar{:else}<LogIn size={13} /> Conectar ChatGPT{/if}</button>{:else}<code class="ml-auto max-w-full truncate text-[10px] text-zinc-500">{provider.base_url}</code>{/if}
</div>
{#if localError}<p role="alert" class="mb-4 text-xs text-red-300">{localError}</p>{/if}
<section class="overflow-hidden rounded-xl border border-line bg-panel" aria-label="Modelos del proveedor">
  <div class="flex flex-wrap items-center justify-between gap-3 border-b border-line px-4 py-3">
    <div class="flex items-center gap-3"><h2 class="text-sm font-semibold">Modelos</h2><div class="flex rounded-lg border border-line bg-canvas p-0.5" aria-label="Filtrar modelos"><button class={`rounded-md px-2.5 py-1 text-[11px] ${filter === 'all' ? 'bg-elevated text-white' : 'text-muted'}`} onclick={() => filter = 'all'} aria-pressed={filter === 'all'}>Todos <span class="ml-1 opacity-50">{rows.length}</span></button><button class={`rounded-md px-2.5 py-1 text-[11px] ${filter === 'active' ? 'bg-elevated text-white' : 'text-muted'}`} onclick={() => filter = 'active'} aria-pressed={filter === 'active'}>Activos <span class="ml-1 opacity-50">{activeCount}</span></button></div></div>
    <div class="flex flex-wrap items-center gap-2"><div class="relative"><Search size={13} class="pointer-events-none absolute left-2.5 top-2.5 text-muted" /><input aria-label="Buscar modelo" class="w-44 !py-1.5 !pl-8 !text-xs lg:w-52" placeholder="Buscar modelos…" bind:value={search} /></div><button class="icon-btn border border-line" aria-label="Actualizar catálogo" title="Actualizar catálogo" disabled={catalog?.loading} onclick={() => discover(provider, true)}><RotateCw size={14} class={catalog?.loading ? 'animate-spin' : ''} /></button><button class="btn !px-2.5 !py-1.5" onclick={() => editModel()}><Plus size={13} /> ID manual</button></div>
  </div>
  {#if catalog?.error}<div role="alert" class="flex flex-wrap items-center gap-3 border-b border-line bg-amber-400/5 px-4 py-3 text-xs text-amber-200"><Info size={14} /><span class="flex-1">{catalog.error}{#if !provider.has_api_key && provider.kind !== 'chatgpt'} · Configura una API key si tu proveedor la requiere.{/if}</span><button class="underline underline-offset-4" onclick={provider.kind === 'chatgpt' ? connect : () => editing = true} disabled={busy}>{provider.kind === 'chatgpt' ? 'Conectar ChatGPT' : 'Configurar conexión'}</button></div>{/if}
  {#if catalog?.warning}<div role="status" class="border-b border-line px-4 py-3 text-xs text-amber-200">{catalog.warning}</div>{/if}
  <div class="overflow-x-auto">
    <table class="w-full whitespace-nowrap text-left text-xs">
      <thead class="border-b border-line bg-canvas/35 text-[10px] uppercase tracking-wider text-muted"><tr><th class="w-10 px-4 py-3"><input type="checkbox" aria-label="Activar todos los modelos visibles" checked={allActive} disabled={!visible.length} onchange={e => { visible.forEach(row => selectModel(provider, row, e.currentTarget.checked)); saveSelection(); }} /></th><th class="min-w-52 py-3 pr-4 font-medium">Modelo</th><th class="px-3 py-3 font-medium">Contexto</th><th class="px-3 py-3 font-medium">Salida máx.</th><th class="px-3 py-3 font-medium">Entrada / salida <span class="block text-[9px] normal-case tracking-normal opacity-60">USD por 1M tokens</span></th><th class="px-3 py-3 font-medium">Capacidades</th><th class="w-10 px-3 py-3"><span class="sr-only">Acciones</span></th></tr></thead>
      <tbody>
        {#each visible as model (model.id)}
          <tr class={`border-b border-line/65 transition-colors last:border-b-0 hover:bg-elevated/40 ${model.selected?.enabled ? 'bg-accent/[.025]' : ''}`}>
            <td class="px-4 py-3"><input type="checkbox" aria-label={`Activar ${model.id}`} checked={!!model.selected?.enabled} onchange={e => toggle(model, e.currentTarget.checked)} /></td>
            <td class="max-w-80 py-3 pr-4"><div class="flex items-center gap-2"><button class="truncate text-left font-medium text-zinc-100 hover:text-accent" onclick={() => { detailId = model.id; detailsOpen = true; }}>{model.selected?.display_name || model.display_name}</button>{#if model.selected?.enabled}<span class="size-1.5 shrink-0 rounded-full bg-accent" title="Activo"></span>{/if}</div><div class="mt-1 flex items-center gap-2"><code class="truncate text-[10px] text-zinc-500">{model.id}</code>{#if !model.available}<span class="text-[9px] text-amber-200/70">Guardado</span>{/if}{#if !model.metadata}<span class="text-[9px] text-zinc-500">Sin metadatos</span>{/if}</div></td>
            <td class="px-3 py-3 font-mono text-zinc-300" title={number(model.metadata?.limit?.context)}>{tokens(model.metadata?.limit?.context)}</td><td class="px-3 py-3 font-mono text-zinc-400" title={number(model.metadata?.limit?.output)}>{tokens(model.metadata?.limit?.output)}</td><td class="px-3 py-3 font-mono text-zinc-300">{price(model.metadata?.cost?.input)} <span class="mx-1 text-zinc-600">/</span> {price(model.metadata?.cost?.output)}</td>
            <td class="px-3 py-3"><div class="flex items-center gap-1.5">{#if model.metadata?.reasoning}<span class="badge !px-1.5" title="Razonamiento" aria-label="Razonamiento"><Brain size={12} class="text-violet-300" /></span>{/if}{#if model.metadata?.tool_call}<span class="badge !px-1.5" title="Herramientas" aria-label="Herramientas"><Wrench size={12} class="text-sky-300" /></span>{/if}{#if model.metadata?.modalities?.input?.includes('image')}<span class="badge !px-1.5" title="Entrada de imágenes" aria-label="Entrada de imágenes"><Image size={12} class="text-amber-300" /></span>{/if}{#if !model.metadata}<span class="text-zinc-600">—</span>{/if}</div></td>
            <td class="px-3 py-3"><DropdownMenu.Root><DropdownMenu.Trigger class="icon-btn" aria-label={`Acciones de ${model.id}`}><MoreHorizontal size={16} /></DropdownMenu.Trigger><DropdownMenu.Portal><DropdownMenu.Content class="menu" sideOffset={4} align="end"><DropdownMenu.Item class="menu-item" onSelect={() => editModel(model)}><SlidersHorizontal size={13} /> Configurar modelo</DropdownMenu.Item><DropdownMenu.Item class="menu-item" onSelect={() => { detailId = model.id; detailsOpen = true; }}><Info size={13} /> Ver metadatos</DropdownMenu.Item></DropdownMenu.Content></DropdownMenu.Portal></DropdownMenu.Root></td>
          </tr>
        {:else}<tr><td colspan="7" class="px-6 py-16 text-center text-muted">{#if catalog?.loading}<LoaderCircle size={20} class="mx-auto mb-3 animate-spin text-accent" /> Consultando catálogo…{:else if search || filter === 'active'}No hay modelos que coincidan con el filtro.{:else}El catálogo aún no está disponible. Configura la conexión o añade un ID manual.{/if}</td></tr>{/each}
      </tbody>
    </table>
  </div>
  <div class="flex flex-wrap justify-between gap-2 border-t border-line bg-canvas/20 px-4 py-3 text-[10px] text-muted"><span>{visible.length} modelos · {activeCount} activos · Guardado automático</span><a href="https://models.dev" target="_blank" rel="noreferrer" class="inline-flex items-center gap-1 hover:text-white">Todos los datos de models.dev <ExternalLink size={10} /></a></div>
</section>
<p class="mt-4 text-[11px] text-zinc-500">Activa los modelos que quieres usar en Claude Code. Después, ejecuta <code class="text-muted">claude-update-models</code> para actualizar su selector.</p>
<ProviderDialog bind:open={editing} {provider} />
<Modal bind:open={modelOpen} title={modelDraft?.original ? 'Configurar modelo' : 'Añadir modelo por ID'} description="El modelo permanece en la misma tabla. Define su nombre y protocolo de conexión.">
  {#if modelDraft}<form class="space-y-4" onsubmit={saveModel}><label>ID del modelo<input required bind:value={modelDraft.id} readonly={!!modelDraft.original} /></label><label>Nombre en Claude Code<input bind:value={modelDraft.display_name} placeholder="Usar nombre del catálogo" /></label><label>Protocolo<select bind:value={modelDraft.protocol} disabled={provider.kind === 'chatgpt' || provider.kind === 'google'}><option value="chat">Chat Completions</option><option value="responses">Responses</option><option value="messages">Anthropic Messages</option></select></label><label class="flex-row items-center"><input type="checkbox" bind:checked={modelDraft.enabled} /> Activo en Claude Code</label>{#if localError}<p role="alert" class="text-xs text-red-300">{localError}</p>{/if}<div class="flex justify-end border-t border-line pt-4"><button class="btn-primary"><Check size={14} /> Guardar modelo</button></div></form>{/if}
</Modal>
<Modal bind:open={detailsOpen} title={detail?.display_name || 'Metadatos'} description="Fuente exclusiva: models.dev. Los IDs disponibles proceden de la API de este proveedor." wide>
  {#if detail}<code class="mb-4 block break-all text-muted">{detail.id}</code>{#if detail.metadata}<div class="mb-5 grid grid-cols-3 gap-3">{#each [['Contexto', detail.metadata.limit?.context], ['Entrada máx.', detail.metadata.limit?.input], ['Salida máx.', detail.metadata.limit?.output]] as [label, value]}<div class="rounded-lg border border-line bg-canvas p-3"><p class="mb-1 text-[10px] text-muted">{label}</p><p class="font-mono text-sm">{number(value)}</p></div>{/each}</div>{#if detail.metadata.description}<p class="mb-5 text-xs leading-relaxed text-muted">{detail.metadata.description}</p>{/if}
<div class="mb-5 grid grid-cols-2 gap-3"><div class="rounded-lg border border-line p-3"><p class="mb-1 text-[10px] text-muted">Entrada · USD / 1M tokens</p><p class="font-mono">{price(detail.metadata.cost?.input)}</p></div><div class="rounded-lg border border-line p-3"><p class="mb-1 text-[10px] text-muted">Salida · USD / 1M tokens</p><p class="font-mono">{price(detail.metadata.cost?.output)}</p></div></div>
<div class="mb-5 flex flex-wrap gap-2">{#if detail.metadata.reasoning}<span class="badge"><Brain size={12} class="text-violet-300" /> Razonamiento</span>{/if}{#if detail.metadata.tool_call}<span class="badge"><Wrench size={12} class="text-sky-300" /> Herramientas</span>{/if}{#each detail.metadata.modalities?.input || [] as modality}<span class="badge">Entrada: {({text:'Texto',image:'Imagen',pdf:'PDF',audio:'Audio',video:'Vídeo'})[modality] || modality}</span>{/each}</div>
<details class="border-t border-line pt-4"><summary class="cursor-pointer text-[11px] text-muted hover:text-white">Ver ficha completa de models.dev</summary><pre class="mt-3 max-h-64 overflow-auto rounded-xl border border-line bg-canvas p-4 font-mono text-[11px] leading-relaxed text-zinc-400">{JSON.stringify(detail.metadata, null, 2)}</pre></details>{:else}<p class="rounded-lg border border-line bg-canvas p-4 text-xs text-muted">No hay una coincidencia en models.dev. Puedes activar este modelo igualmente; no se inventan sus límites ni capacidades.</p>{/if}{/if}
</Modal>
<Modal bind:open={deleting} title="Eliminar proveedor" description={`Se eliminará ${provider.name} de la configuración, junto con su selección de modelos y credenciales guardadas.`}><div class="flex justify-end gap-2"><button class="btn" onclick={() => deleting = false}>Cancelar</button><button class="btn border-red-400/30 text-red-300" disabled={busy} onclick={remove}><Trash2 size={14} /> Eliminar proveedor</button></div></Modal>
