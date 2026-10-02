/**
 * 可用模型获取
 */

import { apiClient } from './client';
import { guardConfigConnection } from './configValue';
import { normalizeModelList } from '@/utils/models';
import { normalizeApiBase } from '@/utils/connection';
import { apiCallApi, getApiCallErrorMessage } from './apiCall';
import { isRecord } from '@/utils/helpers';

const DEFAULT_CLAUDE_BASE_URL = 'https://api.anthropic.com';
const DEFAULT_GEMINI_BASE_URL = 'https://generativelanguage.googleapis.com';
const DEFAULT_ANTHROPIC_VERSION = '2023-06-01';
const CLAUDE_MODELS_IN_FLIGHT = new Map<string, Promise<ReturnType<typeof normalizeModelList>>>();
const GEMINI_MODELS_IN_FLIGHT = new Map<string, Promise<ReturnType<typeof normalizeModelList>>>();

const buildRequestSignature = (
  url: string,
  headers: Record<string, string>,
  authIndex?: string,
  proxyUrl?: string
) => {
  const headerSignature = Object.entries(headers)
    .sort(([a], [b]) => a.toLowerCase().localeCompare(b.toLowerCase()))
    .map(([key, value]) => `${key}:${value}`)
    .join('|');
  return JSON.stringify([url, headerSignature, authIndex ?? '', proxyUrl ?? '']);
};

const buildModelsEndpoint = (baseUrl: string): string => {
  const normalized = normalizeApiBase(baseUrl);
  if (!normalized) return '';
  const trimmed = normalized.replace(/\/+$/g, '');
  if (/\/models$/i.test(trimmed)) return trimmed;
  return `${trimmed}/models`;
};

const buildV1ModelsEndpoint = (baseUrl: string): string => {
  const normalized = normalizeApiBase(baseUrl);
  if (!normalized) return '';
  const trimmed = normalized.replace(/\/+$/g, '');
  if (/\/v1\/models$/i.test(trimmed)) return trimmed;
  if (/\/v1$/i.test(trimmed)) return `${trimmed}/models`;
  return `${trimmed}/v1/models`;
};

/** Use the backend listener behind a gateway, without sending its key to that gateway. */
const buildManagementModelsEndpoint = (baseUrl: string, config: unknown): string => {
  const server = isRecord(config) && isRecord(config.server) ? config.server : null;
  const port = server?.port;
  // TLS listeners need their public hostname for certificate verification. Missing listener
  // settings also retain the public endpoint, but the request still uses management transport.
  if (
    !server ||
    (isRecord(server.tls) && server.tls.enable === true) ||
    typeof port !== 'number' ||
    !Number.isInteger(port) ||
    port < 1 ||
    port > 65535
  ) {
    return buildV1ModelsEndpoint(baseUrl);
  }
  let host = typeof server.host === 'string' ? server.host.trim() : '';
  if (!host || host === '0.0.0.0') host = '127.0.0.1';
  if (host === '::' || host === '[::]') host = '::1';
  if (host.includes(':') && !host.startsWith('[')) host = `[${host}]`;
  return new URL('/v1/models', `http://${host}:${port}`).href;
};

const buildClaudeModelsEndpoint = (baseUrl: string): string => {
  const normalized = normalizeApiBase(baseUrl);
  const fallback = normalized || DEFAULT_CLAUDE_BASE_URL;
  let trimmed = fallback.replace(/\/+$/g, '');
  trimmed = trimmed.replace(/\/v1\/models$/i, '');
  trimmed = trimmed.replace(/\/v1(?:\/.*)?$/i, '');
  return `${trimmed}/v1/models`;
};

const buildGeminiModelsEndpoint = (baseUrl: string): string => {
  const normalized = normalizeApiBase(baseUrl);
  const fallback = normalized || DEFAULT_GEMINI_BASE_URL;
  let trimmed = fallback.replace(/\/+$/g, '');
  trimmed = trimmed.replace(/\/v1beta\/models$/i, '');
  trimmed = trimmed.replace(/\/v1beta(?:\/.*)?$/i, '');
  return `${trimmed}/v1beta/models`;
};

const stripGeminiModelResourceName = (value: string): string => {
  const trimmed = String(value ?? '').trim();
  if (!trimmed) return '';
  return trimmed.replace(/^\/?models\//i, '');
};

const hasHeader = (headers: Record<string, string>, name: string) => {
  const target = name.toLowerCase();
  return Object.keys(headers).some((key) => key.toLowerCase() === target);
};

const resolveBearerTokenFromAuthorization = (headers: Record<string, string>): string => {
  const entry = Object.entries(headers).find(([key]) => key.toLowerCase() === 'authorization');
  if (!entry) return '';
  const value = String(entry[1] ?? '').trim();
  if (!value) return '';
  const match = value.match(/^Bearer\s+(.+)$/i);
  return match?.[1]?.trim() || '';
};

export const modelsApi = {
  /**
   * Fetch available models from /v1/models endpoint (for system info page)
   */
  async fetchModels(baseUrl: string, apiKey?: string, headers: Record<string, string> = {}) {
    if (!buildV1ModelsEndpoint(baseUrl)) throw new Error('Invalid base url');
    const assertConnection = guardConfigConnection();
    const config = await apiClient.get<unknown>('/config');
    assertConnection();
    const endpoint = buildManagementModelsEndpoint(baseUrl, config);
    const resolvedHeaders = { ...headers };
    if (apiKey && !hasHeader(resolvedHeaders, 'authorization')) {
      resolvedHeaders.Authorization = `Bearer ${apiKey}`;
    }
    const result = await apiCallApi.request({
      method: 'GET',
      url: endpoint,
      proxy_url: 'direct',
      header: Object.keys(resolvedHeaders).length ? resolvedHeaders : undefined,
    });
    assertConnection();
    if (result.statusCode < 200 || result.statusCode >= 300) {
      throw new Error(getApiCallErrorMessage(result));
    }
    return normalizeModelList(result.body ?? result.bodyText, { dedupe: true });
  },

  /**
   * Fetch models from /v1/models endpoint via api-call.
   * Useful when the configured baseUrl is the upstream host root (e.g. https://api.example.com).
   */
  async fetchV1ModelsViaApiCall(
    baseUrl: string,
    apiKey?: string,
    headers: Record<string, string> = {},
    authIndex?: string,
    proxyUrl?: string
  ) {
    const endpoint = buildV1ModelsEndpoint(baseUrl);
    if (!endpoint) {
      throw new Error('Invalid base url');
    }

    const trimmedAuthIndex = authIndex?.trim() || undefined;
    const resolvedHeaders = { ...headers };
    if (apiKey && !hasHeader(resolvedHeaders, 'authorization')) {
      resolvedHeaders.Authorization = `Bearer ${apiKey}`;
    } else if (trimmedAuthIndex && !hasHeader(resolvedHeaders, 'authorization')) {
      resolvedHeaders.Authorization = 'Bearer $TOKEN$';
    }

    const result = await apiCallApi.request({
      authIndex: trimmedAuthIndex,
      proxy_url: proxyUrl?.trim() || undefined,
      method: 'GET',
      url: endpoint,
      header: Object.keys(resolvedHeaders).length ? resolvedHeaders : undefined,
    });

    if (result.statusCode < 200 || result.statusCode >= 300) {
      throw new Error(getApiCallErrorMessage(result));
    }

    const payload = result.body ?? result.bodyText;
    return normalizeModelList(payload, { dedupe: true });
  },

  /**
   * Fetch models from /models endpoint via api-call (for OpenAI provider discovery)
   */
  async fetchModelsViaApiCall(
    baseUrl: string,
    apiKey?: string,
    headers: Record<string, string> = {},
    authIndex?: string,
    proxyUrl?: string
  ) {
    const endpoint = buildModelsEndpoint(baseUrl);
    if (!endpoint) {
      throw new Error('Invalid base url');
    }

    const trimmedAuthIndex = authIndex?.trim() || undefined;
    const resolvedHeaders = { ...headers };
    if (apiKey && !hasHeader(resolvedHeaders, 'authorization')) {
      resolvedHeaders.Authorization = `Bearer ${apiKey}`;
    } else if (trimmedAuthIndex && !hasHeader(resolvedHeaders, 'authorization')) {
      resolvedHeaders.Authorization = 'Bearer $TOKEN$';
    }

    const result = await apiCallApi.request({
      authIndex: trimmedAuthIndex,
      proxy_url: proxyUrl?.trim() || undefined,
      method: 'GET',
      url: endpoint,
      header: Object.keys(resolvedHeaders).length ? resolvedHeaders : undefined,
    });

    if (result.statusCode < 200 || result.statusCode >= 300) {
      throw new Error(getApiCallErrorMessage(result));
    }

    const payload = result.body ?? result.bodyText;
    return normalizeModelList(payload, { dedupe: true });
  },

  /**
   * Fetch Claude models from /v1/models via api-call.
   * Anthropic requires `x-api-key` and `anthropic-version` headers.
   */
  async fetchClaudeModelsViaApiCall(
    baseUrl: string,
    apiKey?: string,
    headers: Record<string, string> = {},
    authIndex?: string,
    proxyUrl?: string
  ) {
    const endpoint = buildClaudeModelsEndpoint(baseUrl);
    if (!endpoint) {
      throw new Error('Invalid base url');
    }

    const trimmedAuthIndex = authIndex?.trim() || undefined;
    const resolvedHeaders = { ...headers };
    let resolvedApiKey = String(apiKey ?? '').trim();
    if (!resolvedApiKey && !hasHeader(resolvedHeaders, 'x-api-key')) {
      resolvedApiKey = resolveBearerTokenFromAuthorization(resolvedHeaders);
    }

    if (resolvedApiKey && !hasHeader(resolvedHeaders, 'x-api-key')) {
      resolvedHeaders['x-api-key'] = resolvedApiKey;
    } else if (trimmedAuthIndex && !hasHeader(resolvedHeaders, 'x-api-key')) {
      resolvedHeaders['x-api-key'] = '$TOKEN$';
    }
    if (!hasHeader(resolvedHeaders, 'anthropic-version')) {
      resolvedHeaders['anthropic-version'] = DEFAULT_ANTHROPIC_VERSION;
    }

    const signature = buildRequestSignature(
      endpoint,
      resolvedHeaders,
      trimmedAuthIndex,
      proxyUrl?.trim() || undefined
    );
    const existing = CLAUDE_MODELS_IN_FLIGHT.get(signature);
    if (existing) return existing;

    const request = (async () => {
      const result = await apiCallApi.request({
        authIndex: trimmedAuthIndex,
        proxy_url: proxyUrl?.trim() || undefined,
        method: 'GET',
        url: endpoint,
        header: Object.keys(resolvedHeaders).length ? resolvedHeaders : undefined,
      });

      if (result.statusCode < 200 || result.statusCode >= 300) {
        throw new Error(getApiCallErrorMessage(result));
      }

      const payload = result.body ?? result.bodyText;
      return normalizeModelList(payload, { dedupe: true });
    })();

    CLAUDE_MODELS_IN_FLIGHT.set(signature, request);
    try {
      return await request;
    } finally {
      CLAUDE_MODELS_IN_FLIGHT.delete(signature);
    }
  },

  /**
   * Fetch Gemini models from /v1beta/models via api-call.
   * Gemini API accepts API key via query param or `x-goog-api-key` header.
   */
  async fetchGeminiModelsViaApiCall(
    baseUrl: string,
    apiKey?: string,
    headers: Record<string, string> = {},
    authIndex?: string,
    proxyUrl?: string
  ) {
    const endpoint = buildGeminiModelsEndpoint(baseUrl);
    if (!endpoint) {
      throw new Error('Invalid base url');
    }

    const trimmedAuthIndex = authIndex?.trim() || undefined;
    const resolvedHeaders = { ...headers };
    const resolvedApiKey = String(apiKey ?? '').trim();
    if (resolvedApiKey && !hasHeader(resolvedHeaders, 'x-goog-api-key')) {
      resolvedHeaders['x-goog-api-key'] = resolvedApiKey;
    } else if (trimmedAuthIndex && !hasHeader(resolvedHeaders, 'x-goog-api-key')) {
      resolvedHeaders['x-goog-api-key'] = '$TOKEN$';
    }

    const signature = buildRequestSignature(
      endpoint,
      resolvedHeaders,
      trimmedAuthIndex,
      proxyUrl?.trim() || undefined
    );
    const existing = GEMINI_MODELS_IN_FLIGHT.get(signature);
    if (existing) return existing;

    const request = (async () => {
      const seen = new Set<string>();
      const collected: ReturnType<typeof normalizeModelList> = [];
      let pageToken = '';

      for (let page = 0; page < 20; page += 1) {
        const url = new URL(endpoint);
        if (pageToken) {
          url.searchParams.set('pageToken', pageToken);
        }

        const result = await apiCallApi.request({
          authIndex: trimmedAuthIndex,
          proxy_url: proxyUrl?.trim() || undefined,
          method: 'GET',
          url: url.toString(),
          header: Object.keys(resolvedHeaders).length ? resolvedHeaders : undefined,
        });

        if (result.statusCode < 200 || result.statusCode >= 300) {
          throw new Error(getApiCallErrorMessage(result));
        }

        const payload = result.body ?? result.bodyText;
        const normalized = normalizeModelList(payload, { dedupe: false });
        normalized.forEach((model) => {
          const name = stripGeminiModelResourceName(model.name);
          const key = (name || '').toLowerCase();
          if (!key || seen.has(key)) return;
          seen.add(key);
          const resolved = { ...model, name };
          if (resolved.alias && resolved.alias.trim() === name) {
            resolved.alias = undefined;
          }
          collected.push(resolved);
        });

        const nextToken =
          isRecord(payload) && typeof payload.nextPageToken === 'string'
            ? payload.nextPageToken
            : '';
        if (!nextToken) {
          break;
        }
        pageToken = nextToken;
      }

      return collected;
    })();

    GEMINI_MODELS_IN_FLIGHT.set(signature, request);
    try {
      return await request;
    } finally {
      GEMINI_MODELS_IN_FLIGHT.delete(signature);
    }
  },
};
