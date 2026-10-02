import { afterEach, expect, spyOn, test } from 'bun:test';
import axios from 'axios';
import { apiClient } from '@/services/api/client';
import { modelsApi } from '@/services/api/models';

const originalGet = apiClient.get;
const originalPost = apiClient.post;
const originalDirectGet = axios.get;
afterEach(() => {
  apiClient.get = originalGet;
  apiClient.post = originalPost;
  axios.get = originalDirectGet;
});

const response = { status_code: 200, body: { data: [{ id: 'fixture-model' }] } };

test.each([
  ['', 'http://127.0.0.1:8317/v1/models'],
  ['0.0.0.0', 'http://127.0.0.1:8317/v1/models'],
  ['::', 'http://[::1]:8317/v1/models'],
  ['127.0.0.1', 'http://127.0.0.1:8317/v1/models'],
])('model discovery uses management transport and backend listener %s', async (host, url) => {
  const direct = spyOn(axios, 'get').mockRejectedValue(
    new Error('Public gateway requires a separate key')
  );
  spyOn(apiClient, 'get').mockResolvedValue({ server: { host, port: 8317 } });
  const post = spyOn(apiClient, 'post').mockResolvedValue(response);
  expect(
    (await modelsApi.fetchModels('https://gateway.example', 'fixture-local-key')).map(
      (model) => model.name
    )
  ).toEqual(['fixture-model']);
  expect(direct).not.toHaveBeenCalled();
  expect(post.mock.calls[0][0]).toBe('/requests/api-call');
  expect(post.mock.calls[0][1]).toMatchObject({
    method: 'GET',
    url,
    proxy_url: 'direct',
    header: { Authorization: 'Bearer fixture-local-key' },
  });
});

test('a connection switch during discovery cannot send old credentials to the new server', async () => {
  const pending = Promise.withResolvers<unknown>();
  spyOn(apiClient, 'get').mockReturnValue(pending.promise);
  spyOn(axios, 'get').mockRejectedValue(new Error('Unexpected public request'));
  const post = spyOn(apiClient, 'post').mockResolvedValue(response);
  apiClient.setConfig({ apiBase: 'https://old.example', managementKey: 'fixture' });
  const result = modelsApi.fetchModels('https://old.example', 'old-api-key');
  apiClient.setConfig({ apiBase: 'https://new.example', managementKey: 'fixture-new' });
  pending.resolve({ server: { port: 8317 } });
  await expect(result).rejects.toMatchObject({ name: 'AbortError' });
  expect(post).not.toHaveBeenCalled();
});

test.each([{ server: { port: 8317, tls: { enable: true } } }, { server: { port: 0 } }, {}])(
  'retains the public TLS hostname when direct HTTP listener settings are unavailable',
  async (config) => {
    spyOn(apiClient, 'get').mockResolvedValue(config);
    const post = spyOn(apiClient, 'post').mockResolvedValue(response);
    await modelsApi.fetchModels('https://gateway.example', 'fixture');
    expect(post.mock.calls[0][1]).toMatchObject({
      url: 'https://gateway.example/v1/models',
      proxy_url: 'direct',
    });
  }
);

test('reports model endpoint errors instead of silently returning an empty list', async () => {
  spyOn(apiClient, 'get').mockResolvedValue({ server: { port: 8317 } });
  spyOn(apiClient, 'post').mockResolvedValue({
    status_code: 401,
    body: { error: 'Invalid API key' },
  });
  await expect(modelsApi.fetchModels('https://gateway.example', 'fixture')).rejects.toThrow(
    '401 Invalid API key'
  );
});
