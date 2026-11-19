import '@testing-library/jest-dom/vitest'
import { beforeEach } from 'vitest'

import '../i18n'

// jsdom's localStorage is origin-dependent; tests get a deterministic one.
const store = new Map<string, string>()
Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => void store.set(key, String(value)),
    removeItem: (key: string) => void store.delete(key),
    clear: () => store.clear(),
    key: (index: number) => Array.from(store.keys())[index] ?? null,
    get length() {
      return store.size
    },
  } satisfies Storage,
})

beforeEach(() => localStorage.clear())

// jsdom has no canvas: its getContext returns null after printing a "not
// implemented" notice per call. Same null, no noise; a test that draws spies.
HTMLCanvasElement.prototype.getContext = () => null
