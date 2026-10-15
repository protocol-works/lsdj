# 0036. Curation metadata in the library registry; playlists as m3u8 files

- **Status:** Proposed
- **Date:** 2026-10-14
- **Deciders:** Jake Hartnell

## Context

The Media Explorer's two libraries (generated songs, generated samples) are a
folder of audio files plus a `registry.json` that carries the provenance the
filesystem can't — title, prompt, engine, recipe — reconciled against the folder
on every list (ADR-0022). With ~80 generated songs the library has outgrown a
newest-first list: the DJ needs to mark keepers, name takes after the fact,
describe material in their own words ("drone", "banger"), and sequence tracks
into sets. See [`docs/media-curation-plan.md`](../media-curation-plan.md) for
the feature plan and UX.

The forces:

- The shell owns library state (ADR-0020, ADR-0030): the webview never touches
  the filesystem and the MCP co-DJ reads the same rows the UI shows, so
  curation signals must live where both can see them.
- A song's on-disk filename is the identity everything else holds — an MCP
  agent mid-set, a deck's `TrackSource`, the explorer's re-list keyed by file,
  the in-session row's id and bytes. Changing it under a rename would churn all
  of those.
- The folder watcher must not loop: it already ignores `registry.json` because
  reconcile-on-list rewrites that file.
- Sets are the one artefact a DJ carries elsewhere (a USB stick, another tool),
  so a portable format has value beyond the app.

## Decision

We will store **rating** (1–5), **tags** (a normalised word list), and the
editable **title** on the existing registry rows of both libraries — no second
store — and treat a rename as a registry write that **never renames the file**.
Validation and normalisation happen once, shell-side, and every mutation
returns the updated row so the webview adopts the shell's values.

We will store **playlists** as one standard `.m3u8` file per playlist inside
`generated_songs/playlists/`, with relative paths and `#EXTINF` titles, read
and written by the shell under the songs library's lock; playlist contents are
reconciled against the songs folder on every list, and deleting a song removes
it from every playlist.

Shell-side writers the watcher cannot see (MCP tools) emit `library://changed`
themselves.

## Consequences

- Rating, tags, and title round-trip through one row type per library; MCP
  `list_songs`/`list_samples` expose them with no extra work, and the co-DJ can
  bias its choices on them.
- A rename is instant and race-free, but the filename shown by "Open songs
  folder" keeps the original stem. A one-shot "rename files to match titles"
  maintenance command is the shape of any future fix.
- A hand-renamed or hand-moved file loses its curation exactly as it loses its
  prompt today; the folder stays the source of truth.
- Playlists are hand-editable, Finder-visible, and importable elsewhere; the
  cost is a tiny m3u8 parser/writer and a reconcile step. Playlist order is the
  rotary's browse order, so a set can be walked from the hardware.
- The webview's re-list must rebuild row fields from the registry (keeping only
  its session id and in-memory bytes) so an out-of-band write is not masked by
  a reused row.
- Folder-tab files (no registry) stay outside curation; the planned "Import to
  library" action is the way in.

## Alternatives considered

- **A sidecar curation file or webview localStorage** — a second source of
  truth the shell and MCP would have to merge; the registry already is the
  per-row metadata store.
- **Renaming the file with the title** — churns every holder of the filename
  (MCP, deck source, re-list, session rows) for a cosmetic gain.
- **Playlists as a single `playlists.json`** — marginally less code, but
  nothing else gains; m3u8 is standard and portable.
- **Tags as a separate entity with colours/hierarchy** — speculative; the
  vocabulary is derived from use, and a `#tag` search plus sort already acts as
  a smart playlist.
