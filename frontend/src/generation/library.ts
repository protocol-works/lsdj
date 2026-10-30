/** The songs library write path, shared by the explorer's Generate tab and the
 * loom's render queue: a rendered take becomes a registry row. */
import { encodeMetaFrame, invoke } from '../audio/nativeEngine'
import type { SongGenerationEngine, SongGenerationRecipeV1 } from './songGeneration'

// One row of the on-disk song registry (Rust `songs::SongEntry`, camelCase): a file
// in the songs folder with the provenance the filesystem can't carry.
export type SongEntry = {
  file: string
  title: string
  prompt: string | null
  model: string | null
  recipe?: unknown
  /** 1–5; absent when unrated. */
  rating?: number
  /** Absent when untagged. */
  tags?: string[]
  /** Background analysis (Rust `analysis::media::TrackInfo`); absent until it
   * runs, and when it wasn't confident. `key` is Camelot ("8A"). */
  bpm?: number
  key?: string
  keyName?: string
}

/** What the webview sends with a freshly composed take (Rust `songs::NewSong`). */
export type NewSong = {
  title: string
  prompt: string
  model: SongGenerationEngine
  recipe: SongGenerationRecipeV1
}

/** Persist a ready take to ~/Documents/LSDJ/generated_songs through the Rust shell
 * and return its registry entry ({@link encodeMetaFrame} builds the binary payload —
 * a JSON args map would be megabytes of text for a multi-MB WAV). The old
 * `<a download>` is gone: it silently no-ops in WKWebView. */
export function saveGeneratedSong(meta: NewSong, wav: ArrayBuffer): Promise<SongEntry> {
  return invoke<SongEntry>('save_generated_song', encodeMetaFrame(meta, wav))
}
