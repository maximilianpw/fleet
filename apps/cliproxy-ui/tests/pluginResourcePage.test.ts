import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { resolvePluginAssetURL, resolvePluginPageURL } from '../src/features/plugins/pluginResources';

const apiBase = 'https://proxy.example.test';

describe('plugin resource page', () => {
  test('plugin pages resolve only http(s) URLs and server paths', () => {
    expect(resolvePluginPageURL('/plugins/demo/index.html', apiBase)).toBe(
      'https://proxy.example.test/plugins/demo/index.html'
    );
    expect(resolvePluginPageURL('https://plugin.example.test/page', apiBase)).toBe(
      'https://plugin.example.test/page'
    );
    for (const value of [
      'data:text/html,x',
      'blob:https://proxy.example.test/1',
      'javascript:alert(1)',
      '//evil.example.test/x',
      'relative/page.html',
      '',
    ]) {
      expect(resolvePluginPageURL(value, apiBase)).toBe('');
    }
  });

  test('logos still accept data URLs', () => {
    const logo = 'data:image/png;base64,AAAA';
    expect(resolvePluginAssetURL(logo, apiBase)).toBe(logo);
  });

  test('plugin frame is sandboxed without same-origin access or clipboard read', () => {
    const source = readFileSync(
      new URL('../src/features/plugins/PluginResourcePage.tsx', import.meta.url),
      'utf8'
    );
    const sandbox = /sandbox="([^"]*)"/.exec(source);
    expect(sandbox?.[1]).toStartWith('allow-scripts');
    expect(sandbox?.[1]).not.toContain('allow-same-origin');
    expect(source).not.toContain('clipboard-read');
  });
});
