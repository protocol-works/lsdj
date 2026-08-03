# LSDJ frontend

The React deck UI (Vite + React + TypeScript). Setup, run, and verification
commands live in the root [`justfile`](../justfile) — see the
[project README](../README.md).

- `src/deck/` — deck state reducer (`deckState.ts`), WebSocket + Web Audio
  hook (`useDeck.ts`), and the deck panel UI
- `src/ui/` — design tokens (`tokens.css`) and the component kit; components
  style only from tokens
- `src/i18n/` — all user-facing strings, keyed by intent
- `public/player-worklet.js` — AudioWorklet ring buffer the deck streams into
- `scripts/verify_m2.mjs` — headless-browser check of the M2 exit criteria
  against a running backend
