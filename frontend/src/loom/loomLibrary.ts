/**
 * Shell-side loom files and the node cue (architecture §4): thin typed `invoke`s.
 * The shell stores a loom opaquely in `generated_songs/looms/<id>.json`; the
 * caller validates what `readLoom` returns (`parseLoom`). No library event fires
 * for looms, so refresh the list after `saveLoom`/`deleteLoom` resolve.
 */
import { invoke } from '../audio/nativeEngine'
import { seedState } from './fixture'
import { persistable } from './loomState'

/** One loom as the shell lists it (`LoomSummary`, camelCase over IPC). */
export type LoomSummary = { id: string; name: string; updatedAt: number; nodes: number }

/** The visual harness: `npx vite` plus `?loomFixture` boots the loom on the
 * mockup's seed graph without the shell, for side-by-side looks at the skin.
 * Dev builds only; a save still goes to IPC, so in a browser it fails. */
const FIXTURE = import.meta.env.DEV && location.search.includes('loomFixture')
const fixtureLoom = () => ({ ...persistable(seedState(), Date.now()), createdAt: Date.now() })

/** Every loom, newest first. */
export function listLooms(): Promise<LoomSummary[]> {
  if (FIXTURE) {
    const loom = fixtureLoom()
    return Promise.resolve([{ id: loom.id, name: loom.name, updatedAt: loom.updatedAt, nodes: loom.nodes.length }])
  }
  return invoke<LoomSummary[]>('list_looms')
}

/** One loom's stored document, unvalidated. */
export function readLoom(id: string): Promise<unknown> {
  if (FIXTURE) return Promise.resolve(fixtureLoom())
  return invoke<unknown>('read_loom', { id })
}

/** Create or replace a loom. The shell validates the id and name, caps the size
 * at 2 MiB, stamps `name` (and `updatedAt` when absent), and writes atomically. */
export function saveLoom(id: string, name: string, loom: unknown): Promise<LoomSummary> {
  return invoke<LoomSummary>('save_loom', { id, name, loom })
}

/** Move a loom to the OS Trash; the songs it names stay. */
export function deleteLoom(id: string): Promise<void> {
  return invoke('delete_loom', { id })
}

/** Cue `start..end` seconds (`end` omitted = to the end) of a library song in
 * the phones, looping; `audition_stop` stops it. Never touches the master. */
export function auditionSongClip(name: string, start: number, end?: number): Promise<void> {
  return invoke('audition_song_clip', { name, startSeconds: start, endSeconds: end ?? null })
}
