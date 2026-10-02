import { afterEach, beforeEach, expect, spyOn, test } from 'bun:test';
import { AxiosError, AxiosHeaders, type AxiosResponse } from 'axios';
import { apiClient } from '../src/services/api/client';
import { configApi } from '../src/services/api/config';
import { useAuthStore } from '../src/stores/useAuthStore';
import type { Config } from '../src/types';

const storageDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
const windowDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'window');
const originalGetConfig = configApi.getConfig;
let events: EventTarget;

beforeEach(() => {
  const values = new Map<string, string>();
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
    },
  });
  events = Object.assign(new EventTarget(), { location: { host: 'review.example' } });
  Object.defineProperty(globalThis, 'window', { configurable: true, value: events });
  useAuthStore.getState().logout();
});

afterEach(() => {
  configApi.getConfig = originalGetConfig;
  useAuthStore.getState().logout();
  if (storageDescriptor) Object.defineProperty(globalThis, 'localStorage', storageDescriptor);
  else Reflect.deleteProperty(globalThis, 'localStorage');
  if (windowDescriptor) Object.defineProperty(globalThis, 'window', windowDescriptor);
  else Reflect.deleteProperty(globalThis, 'window');
});

const credentials = {
  apiBase: 'https://old.example',
  managementKey: 'fixture-key',
  rememberPassword: true,
};

test('logout invalidates a pending login and does not persist its key', async () => {
  const request = Promise.withResolvers<Config>();
  spyOn(configApi, 'getConfig').mockReturnValue(request.promise);
  const pending = useAuthStore.getState().login(credentials);
  useAuthStore.getState().logout();
  request.resolve({});
  await pending.catch(() => undefined);
  expect(useAuthStore.getState()).toMatchObject({
    isAuthenticated: false,
    managementKey: '',
    connectionStatus: 'disconnected',
  });
  expect(localStorage.getItem('isLoggedIn')).toBeNull();
});

test('an older login cannot replace a newer session', async () => {
  const request = Promise.withResolvers<Config>();
  spyOn(configApi, 'getConfig').mockReturnValueOnce(request.promise).mockResolvedValueOnce({});
  const pending = useAuthStore.getState().login(credentials);
  await useAuthStore.getState().login({ ...credentials, apiBase: 'https://new.example' });
  request.resolve({});
  await pending.catch(() => undefined);
  expect(useAuthStore.getState()).toMatchObject({
    apiBase: 'https://new.example',
    connectionStatus: 'connected',
  });
});

test('a pending auth check cannot resurrect a logged-out session', async () => {
  const request = Promise.withResolvers<Config>();
  spyOn(configApi, 'getConfig').mockReturnValue(request.promise);
  useAuthStore.setState(credentials);
  const pending = useAuthStore.getState().checkAuth();
  useAuthStore.getState().logout();
  request.resolve({});
  expect(await pending).toBe(false);
  expect(useAuthStore.getState().isAuthenticated).toBe(false);
});

test.each([true, false])(
  'only current responses emit global auth/capability events, stale=%s',
  async (stale) => {
    const started = Promise.withResolvers<void>();
    const reply = Promise.withResolvers<AxiosResponse>();
    let unauthorized = 0;
    let versions = 0;
    events.addEventListener('unauthorized', () => unauthorized++);
    events.addEventListener('server-version-update', () => versions++);
    apiClient.setConfig(credentials);
    const pending = apiClient.get('/config', {
      adapter: async (config) => {
        started.resolve();
        const result = await reply.promise;
        const response = { ...result, config };
        if (response.status === 401)
          throw new AxiosError('Unauthorized', 'ERR_BAD_REQUEST', config, undefined, response);
        return response;
      },
    });
    await started.promise;
    if (stale) apiClient.setConfig({ ...credentials, apiBase: 'https://new.example' });
    reply.resolve({
      data: {},
      status: 401,
      statusText: 'Unauthorized',
      headers: new AxiosHeaders(),
      config: { headers: new AxiosHeaders() },
    });
    await expect(pending).rejects.toMatchObject({ status: 401 });
    expect(unauthorized).toBe(stale ? 0 : 1);
    expect(versions).toBe(0);
  }
);

test('stale successful responses cannot update server capabilities after an ABA switch', async () => {
  const started = Promise.withResolvers<void>();
  const reply = Promise.withResolvers<void>();
  const seen: string[] = [];
  for (const name of ['server-version-update', 'server-plugin-support-update'])
    events.addEventListener(name, () => seen.push(name));
  apiClient.setConfig(credentials);
  const pending = apiClient.get('/config', {
    adapter: async (config) => {
      started.resolve();
      await reply.promise;
      return {
        config,
        data: {},
        status: 200,
        statusText: 'OK',
        headers: new AxiosHeaders({ 'x-cpa-version': '8.0.4', 'x-cpa-support-plugin': 'true' }),
      };
    },
  });
  await started.promise;
  apiClient.setConfig({ ...credentials, apiBase: 'https://new.example' });
  apiClient.setConfig(credentials);
  reply.resolve();
  await pending;
  expect(seen).toEqual([]);
});
