import { expect, test } from 'bun:test';
import { resolveActiveProviderBrand } from '@/features/providers/uiState';
import type { ProviderBrand } from '@/features/providers/types';

const groups: { id: ProviderBrand; resources: unknown[] }[] = [
  { id: 'gemini', resources: [] },
  { id: 'codex', resources: [] },
  { id: 'openaiCompatibility', resources: [{}] },
];
test('starts with the first configured provider category', () => {
  expect(resolveActiveProviderBrand(groups, null)).toBe('openaiCompatibility');
});
test('preserves an explicit empty category so users can add its first provider', () => {
  expect(resolveActiveProviderBrand(groups, 'gemini')).toBe('gemini');
});
test('falls back when a saved category is unavailable or all categories are empty', () => {
  expect(resolveActiveProviderBrand(groups, 'claude')).toBe('openaiCompatibility');
  expect(resolveActiveProviderBrand(groups.slice(0, 2), null)).toBe('gemini');
  expect(resolveActiveProviderBrand([], null)).toBe('gemini');
});
