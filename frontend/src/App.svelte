<script>
  import { onMount } from 'svelte';
  import { Boxes, Plus, SlidersHorizontal, ArrowUpRight, CircleCheck, LoaderCircle, RotateCw, ChevronRight, Command } from '@lucide/svelte';
  import { app, route, load, navigate, persist } from './state.svelte.js';
  import { presets } from './catalog.js';
  import ProviderDialog from './ProviderDialog.svelte';
  import ProviderPage from './ProviderPage.svelte';
  import SettingsPage from './SettingsPage.svelte';
  let adding = $state(false);
  let page = $derived(route.path === '/settings' ? 'settings' : route.path.startsWith('/providers/') ? 'provider' : route.path === '/' || route.path === '/providers' ? 'overview' : 'missing');
  let provider = $derived(app.settings?.providers.find(p => `/providers/${p.id}` === route.path));
  let count = $derived(app.settings?.providers.filter(p => p.enabled).reduce((n, p) => n + p.models.filter(m => m.enabled).length, 0) || 0);
  onMount(load);
  function go(event, path) { if (event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) { event.preventDefault(); navigate(path); } }
</script>

<div class="min-h-dvh md:grid md:grid-cols-[220px_minmax(0,1fr)]">
  <aside class="border-b border-line bg-[#13161b] md:sticky md:top-0 md:flex md:h-dvh md:flex-col md:border-b-0 md:border-r">
    <a href="/providers" onclick={e => go(e, '/providers')} class="flex items-center gap-3 px-5 py-6"><span class="flex size-8 items-center justify-center rounded-lg bg-accent text-zinc-950"><Command size={18} strokeWidth={2.5} /></span><span class="text-sm font-semibold tracking-tight">Claude Proxy<span class="ml-2 align-middle text-[9px] uppercase tracking-widest text-muted">local</span></span></a>
    <nav class="flex gap-1 overflow-x-auto px-3 pb-3 md:flex-col md:overflow-visible" aria-label="Navegación principal">
      <a href="/providers" onclick={e => go(e, '/providers')} class={`flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-xs font-medium ${page === 'overview' ? 'bg-elevated text-white' : 'text-muted hover:text-white'}`} aria-current={page === 'overview' ? 'page' : undefined}><Boxes size={16} /> Proveedores<span class="ml-auto text-[10px] text-muted">{app.settings?.providers.length || 0}</span></a>
      <a href="/settings" onclick={e => go(e, '/settings')} class={`flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-xs font-medium ${page === 'settings' ? 'bg-elevated text-white' : 'text-muted hover:text-white'}`} aria-current={page === 'settings' ? 'page' : undefined}><SlidersHorizontal size={16} /> Preferencias</a>
      <div class="my-5 hidden items-center justify-between px-3 md:flex"><span class="eyebrow">Tus conexiones</span><button class="icon-btn -my-2" onclick={() => adding = true} aria-label="Añadir proveedor en barra lateral" disabled={!app.settings}><Plus size={15} /></button></div>
      {#each app.settings?.providers || [] as p}
        <a href={`/providers/${p.id}`} onclick={e => go(e, `/providers/${p.id}`)} class={`flex items-center gap-2.5 rounded-lg px-3 py-2 text-xs ${provider?.id === p.id ? 'bg-accent/8 text-accent' : 'text-muted hover:bg-elevated/50 hover:text-white'}`} aria-current={provider?.id === p.id ? 'page' : undefined}>
          <span class="flex size-6 shrink-0 items-center justify-center rounded-md text-[9px] font-semibold" style={`background:${presets[p.kind].color}10;color:${presets[p.kind].color}`}>{presets[p.kind].mark}</span><span class="truncate">{p.name}</span><span class="ml-auto text-[10px] opacity-60">{p.models.filter(m => m.enabled).length}</span>
        </a>
      {/each}
    </nav>
    <div class="mt-auto hidden border-t border-line px-5 py-5 text-[11px] text-muted md:block"><div class="mb-1 flex items-center gap-2"><span class="size-1.5 rounded-full bg-accent"></span>Configuración local</div><span class="text-zinc-600">Guardada en SQLite</span></div>
  </aside>
  <main class="min-w-0">
    <header class="flex h-14 items-center justify-between border-b border-line px-5 lg:px-8">
      <div class="flex items-center gap-2 text-xs text-muted"><span>Espacio local</span><ChevronRight size={13} /><span class="text-zinc-300">{provider?.name || (page === 'settings' ? 'Preferencias' : 'Proveedores')}</span></div>
      <div class="flex items-center gap-1.5 text-[11px] text-muted" role="status">{#if app.saving}<LoaderCircle size={13} class="animate-spin" /> Guardando…{:else if app.saveError}<span class="text-red-300">Error al guardar</span><button class="ml-1 underline" onclick={() => persist().catch(() => {})}>Reintentar</button>{:else}<CircleCheck size={13} class="text-accent" />{app.lastSaved ? 'Cambios guardados' : 'Sincronizado'}{/if}</div>
    </header>
    {#if app.saveError}<div role="alert" class="border-b border-red-400/20 bg-red-400/5 px-8 py-3 text-xs text-red-300">{app.saveError} · Los cambios pendientes siguen visibles; reintenta guardarlos.</div>{/if}
    <div class="mx-auto max-w-[1600px] p-5 lg:p-8">
      {#if app.loading}<div class="flex items-center gap-2 py-20 text-muted"><LoaderCircle size={18} class="animate-spin" /> Cargando configuración…</div>
      {:else if app.error}<div role="alert" class="py-12"><h1 class="mb-2 text-xl font-semibold">No se pudo cargar la configuración</h1><p class="mb-5 text-muted">{app.error}</p><button class="btn" onclick={load}><RotateCw size={14} /> Reintentar</button></div>
      {:else if page === 'settings'}<SettingsPage />
      {:else if page === 'provider' && provider}{#key provider.id}<ProviderPage {provider} />{/key}
      {:else if page === 'missing' || page === 'provider'}<h1 class="py-12 text-xl">Esta página no existe.</h1><a href="/providers" onclick={e => go(e, '/providers')} class="btn">Volver a proveedores</a>
      {:else}
        <div class="mb-7 flex flex-wrap items-start justify-between gap-4"><div><p class="eyebrow mb-2">Conexiones</p><h1 class="text-2xl font-semibold tracking-tight">Proveedores</h1><p class="mt-2 text-xs text-muted">Un lugar para tus modelos. Cada conexión, su propio catálogo.</p></div><button class="btn-primary" onclick={() => adding = true}><Plus size={15} /> Añadir proveedor</button></div>
        <div class="mb-7 flex gap-8 border-y border-line py-4"><div><span class="text-xl font-semibold">{app.settings.providers.length}</span><span class="ml-2 text-xs text-muted">proveedores</span></div><div><span class="text-xl font-semibold text-accent">{count}</span><span class="ml-2 text-xs text-muted">modelos activos</span></div><span class="ml-auto self-center text-[11px] text-muted">Metadatos de models.dev</span></div>
        {#if app.settings.providers.length}
          <div class="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
            {#each app.settings.providers as p}
              <a href={`/providers/${p.id}`} onclick={e => go(e, `/providers/${p.id}`)} class="group rounded-xl border border-line bg-panel p-5 transition-colors hover:border-zinc-500">
                <div class="mb-5 flex items-center justify-between"><span class="flex size-10 items-center justify-center rounded-xl text-xs font-semibold" style={`background:${presets[p.kind].color}10;color:${presets[p.kind].color}`}>{presets[p.kind].mark}</span><ArrowUpRight size={16} class="text-muted group-hover:text-accent" /></div>
                <h2 class="font-semibold">{p.name}</h2><p class="mt-1 text-xs text-muted">{presets[p.kind].description}</p>
                <div class="mt-5 flex items-center justify-between border-t border-line pt-4"><span class={`badge ${p.enabled ? 'text-accent' : ''}`}><span class={`size-1 rounded-full ${p.enabled ? 'bg-accent' : 'bg-zinc-500'}`}></span>{p.enabled ? 'Activo' : 'Pausado'}</span><span class="text-xs text-muted">{p.models.filter(m => m.enabled).length} modelos</span></div>
              </a>
            {/each}
          </div>
        {:else}
          <div class="rounded-2xl border border-dashed border-line bg-panel/40 px-6 py-14 text-center"><Boxes size={30} class="mx-auto mb-4 text-accent" /><h2 class="text-lg font-semibold">Conecta tu primer proveedor</h2><p class="mx-auto mb-6 mt-2 max-w-sm text-xs leading-relaxed text-muted">OpenCode Go, OpenAI API, ChatGPT, Google o tu propia API. Después, elige tus modelos en una sola tabla.</p><button class="btn-primary" onclick={() => adding = true}><Plus size={14} /> Añadir proveedor</button></div>
        {/if}
        <div class="mt-8 rounded-xl border border-line bg-panel/40 p-4 text-xs text-muted"><div class="flex items-center gap-2 font-medium text-zinc-300"><Command size={15} /> Conectar Claude Code</div><p class="mt-2">Activa tus modelos y ejecuta <code class="text-accent">claude-update-models --endpoint {window.location.origin}</code></p></div>
      {/if}
    </div>
  </main>
</div>
<ProviderDialog bind:open={adding} />
