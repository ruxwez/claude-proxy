import test from 'node:test';
import assert from 'node:assert/strict';
import { modelRows, selectModel } from './catalog.js';

test('selection stays in its catalog row, including after refresh and while absent', () => {
  const provider = { kind: 'openai', models: [] };
  const catalog = [{ id: 'one', display_name: 'One', metadata: { limit: { context: 32000 } }, suggested_protocol: 'responses' }, { id: 'two', display_name: 'Two' }];
  selectModel(provider, catalog[0], true);
  assert.equal(modelRows(provider, catalog).length, 2);
  assert.equal(modelRows(provider, catalog)[0].selected.enabled, true);
  assert.equal(modelRows(provider, catalog)[0].metadata.limit.context, 32000);
  selectModel(provider, catalog[0], false);
  selectModel(provider, catalog[0], true);
  assert.equal(provider.models.length, 1);
  assert.equal(modelRows(provider, [catalog[1]]).length, 2);
  assert.equal(modelRows(provider, [catalog[1]])[1].selected.enabled, true);
});

test('OpenAI API and ChatGPT models use their provider credential exclusively', () => {
  for (const [kind, credential] of [['openai', 'api_key'], ['chatgpt', 'subscription']]) {
    const provider = { kind, models: [] };
    selectModel(provider, { id: 'shared', display_name: 'Shared' }, true);
    assert.equal(provider.models[0].credential, credential);
    assert.equal(provider.models[0].protocol, 'responses');
  }
});
