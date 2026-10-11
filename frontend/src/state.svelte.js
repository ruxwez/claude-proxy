export const app = $state({ settings: null, loading: true, error: '', saving: false, saveError: '', lastSaved: false, catalogs: {}, metadataProviders: [] });
export const route = $state({ path: window.location.pathname });

export function navigate(path) {
  if (path !== window.location.pathname) window.history.pushState({}, '', path);
  route.path = path;
}
window.addEventListener('popstate', () => route.path = window.location.pathname);

export async function api(path, method = 'GET', body) {
  const response = await fetch(path, { method, headers: { 'Content-Type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  const data = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(data.error || `Error HTTP ${response.status}`);
  return data;
}

export async function load() {
  app.loading = true;
  try {
    app.settings = await api('/api/settings');
    app.error = '';
    if (new URLSearchParams(window.location.search).has('connected')) app.lastSaved = true;
    api('/api/metadata/providers').then(value => app.metadataProviders = value.data).catch(() => {});
  } catch (error) { app.error = error.message; }
  finally { app.loading = false; }
}

let queue = Promise.resolve();
let pending = 0;
export function persist() {
  const snapshot = JSON.stringify(app.settings);
  pending++;
  app.saving = true;
  app.saveError = '';
  const result = queue.then(() => api('/api/settings', 'PUT', JSON.parse(snapshot)));
  queue = result.catch(() => {});
  return result.then(() => { app.lastSaved = true; app.saveError = ''; }, error => { app.saveError = error.message; throw error; }).finally(() => { pending--; app.saving = pending > 0; });
}

export async function discover(provider, refresh = false) {
  const id = provider.id;
  if (app.catalogs[id]?.loading) return;
  const previous = app.catalogs[id];
  app.catalogs[id] = { data: previous?.data || [], loading: true, error: '', warning: '' };
  try {
    const result = await api('/api/providers/catalog', 'POST', { provider, refresh });
    app.catalogs[id] = { ...result, loading: false, error: '' };
  } catch (error) { app.catalogs[id] = { data: previous?.data || [], loading: false, error: error.message, warning: '' }; }
}
