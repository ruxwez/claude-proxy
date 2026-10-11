export const presets = {
  opencode_go: { name: 'OpenCode Go', base: 'https://opencode.ai/zen/go/v1', description: 'Tu plan de modelos para programar', mark: 'OC', color: '#b4f299' },
  openai: { name: 'OpenAI API', base: 'https://api.openai.com/v1', description: 'Acceso con API key', mark: 'AI', color: '#9cbcff' },
  chatgpt: { name: 'ChatGPT', base: 'https://api.openai.com/v1', description: 'Acceso con tu suscripción', mark: 'CG', color: '#9ce7d3' },
  google: { name: 'Google Gemini', base: 'https://generativelanguage.googleapis.com/v1beta/openai', description: 'Modelos Gemini con API key', mark: 'G', color: '#efc689' },
  openai_compatible: { name: 'API compatible', base: 'http://localhost:8000/v1', description: 'Tu endpoint compatible con OpenAI', mark: '</>', color: '#c6a7ef' }
};

export const credential = provider => provider.kind === 'chatgpt' ? 'subscription' : 'api_key';
export const protocol = provider => ['openai', 'chatgpt'].includes(provider.kind) ? 'responses' : 'chat';
export const number = value => typeof value === 'number' ? new Intl.NumberFormat('es-ES').format(value) : '—';
export const tokens = value => typeof value !== 'number' ? '—' : value >= 1_000_000 ? `${+(value / 1_000_000).toFixed(2)}M` : value >= 1000 ? `${+(value / 1000).toFixed(1)}K` : String(value);
export const price = value => typeof value === 'number' ? `$${value}` : '—';

// Saved, manual and temporarily absent models stay in the same table.
export function modelRows(provider, catalog = []) {
  const rows = new Map(catalog.map(model => [model.id, { ...model, available: true }]));
  for (const model of provider.models) {
    if (!rows.has(model.id)) rows.set(model.id, { id: model.id, display_name: model.display_name || model.id, metadata: null, match: 'none', available: false });
  }
  return [...rows.values()].map(row => ({ ...row, selected: provider.models.find(model => model.id === row.id) }));
}

export function selectModel(provider, row, enabled) {
  const selected = provider.models.find(model => model.id === row.id);
  if (selected) selected.enabled = enabled;
  else if (enabled) provider.models.push({ id: row.id, display_name: row.display_name, enabled: true, protocol: row.suggested_protocol || protocol(provider), credential: credential(provider) });
}
