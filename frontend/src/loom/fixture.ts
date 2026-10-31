/** Test fixture: the loom mockup's seed graph (`mockups/loom/index.html`),
 * every node rendered with a clip of exactly its kept tail plus its bars. It is
 * also the visual harness's library (`fixtureInvoke`). */

import type { SongEntry } from '../generation/library'
import type { LoraChoice } from '../models/useLoras'
import { initialLoomState, persistable, type Loom, type LoomNode, type LoomState } from './loomState'
import { barSeconds, TAIL_BARS } from './timing'

export function fixtureNode(
  id: string,
  parentId: string | null,
  prompt: string,
  bars: number,
  bpm: number,
  extra: Partial<LoomNode> = {},
): LoomNode {
  const tailSeconds = parentId === null ? 0 : TAIL_BARS * barSeconds(bpm)
  return {
    id,
    parentId,
    prompt,
    bars,
    bpm,
    seed: 0,
    loras: [],
    sa3: {},
    file: `${id}.wav`,
    durationSeconds: tailSeconds + bars * barSeconds(bpm),
    tailSeconds,
    in: 0,
    out: bars,
    takeGroup: null,
    status: 'ready',
    auto: false,
    createdAt: 0,
    ...extra,
  }
}

const acid: LoraChoice[] = [{ name: 'industrial-techno', strength: 1 }]
const idm: LoraChoice[] = [{ name: 'colundi-idm', strength: 0.8 }]

export function seedLoom(): Loom {
  return {
    version: 1,
    id: 'loom-fixture',
    name: 'Late dub',
    createdAt: 0,
    updatedAt: 0,
    nodes: [
      fixtureNode('r1', null, 'ambient dub, tape delay, submerged', 16, 124, { key: '8A' }),
      fixtureNode('n2', 'r1', 'dub techno, rolling bassline', 16, 124, { key: '8A' }),
      fixtureNode('n3', 'n2', 'deep minimal techno, hypnotic, sub pulse', 24, 124, { key: '8A' }),
      fixtureNode('n4', 'n3', 'acid techno, 303 squelch, peak time', 16, 124, { key: '8A', loras: acid }),
      fixtureNode('n5', 'n2', 'deep house, rain on glass, rhodes', 16, 124, { key: '7B' }),
      fixtureNode('n6', 'n5', 'disco strings, four on the floor, sunrise', 16, 124, { key: '6B' }),
      fixtureNode('n7', 'n2', 'breakbeat jungle, chopped amens, reese bass', 16, 124, { key: '8A' }),
      fixtureNode('r2', null, 'warm lo-fi hip hop, vinyl crackle, dusty keys', 16, 86, { key: '7A', loras: idm }),
      fixtureNode('n9', 'r2', 'jazz piano, brushed drums, warm upright', 16, 86, { key: '7A' }),
      fixtureNode('n10', 'n9', 'psychedelic soul, wah guitar, falsetto', 16, 86, { key: '9A' }),
      fixtureNode('r3', null, 'drone, cosmic, space choir', 32, 60, { key: '8B' }),
    ],
  }
}

export const seedState = (): LoomState => initialLoomState(seedLoom())

/** The visual harness: `npx vite` plus `?loomFixture` runs the webview without
 * the shell, its IPC answered by `fixtureInvoke` — the loom boots on the seed
 * graph and the explorer lists songs a drop can use. Dev builds only. */
export const LOOM_FIXTURE = import.meta.env.DEV && location.search.includes('loomFixture')

const song = (file: string, title: string, prompt: string, bpm: number, key: string, seconds: number, rating: number, tags: string[]): SongEntry => ({
  file,
  title,
  prompt,
  model: 'track',
  bpm,
  key,
  rating,
  tags,
  recipe: { version: 1, seconds },
})

/** The harness's songs library: the seed graph's renders, then the mockup
 * explorer's songs, which the newest-first list shows on top. */
function fixtureSongs(): SongEntry[] {
  return [
    ...seedLoom().nodes.map((n) => song(n.file!, n.prompt, n.prompt, n.bpm, n.key ?? '', n.durationSeconds!, 0, [])),
    song('electric-halo.wav', 'Electric Halo', 'psychic techno, midtempo microsound glitch 666 WITCH RAVE', 106, '8A', 240, 1, ['witch', 'glitch']),
    song('golden-aurora.wav', 'Golden Aurora', 'Psychic Techno 666', 133, '7A', 120, 3, ['banger']),
    song('quiet-horizon.wav', 'Quiet Horizon', 'berghain witch rave, pulsing bass, deep rumble kick, colundi alien ritual', 100, '7A', 240, 5, ['banger', 'witch']),
    song('velvet-static.wav', 'Velvet Static', 'ambient dub, tape delay, submerged chords, 124 BPM', 124, '8A', 180, 5, ['ambient']),
    song('alien-opera.wav', 'Alien Opera', 'alien opera', 68, '10A', 120, 4, ['drone']),
  ]
}

const FIXTURE_SAMPLES = [
  { file: 'riser.wav', title: 'Riser', prompt: 'white noise riser, 8 bars', model: 'sfx', oneShot: true, rating: 3, tags: ['fx'] },
  { file: 'dub-chord.wav', title: 'Dub chord', prompt: 'dub techno chord stab, long delay tail', model: 'music', oneShot: false, bpm: 124, key: '8A', tags: ['dub'] },
]

/** The harness's IPC: the looms, the libraries, a song's length (a drop needs
 * it) and a quiet autosave; any other command fails as it does without Tauri. */
export function fixtureInvoke<T>(cmd: string, args?: unknown): Promise<T> {
  const { name } = (args ?? {}) as { name?: string }
  const loom = { ...persistable(seedState(), Date.now()), createdAt: Date.now() }
  const recipe = fixtureSongs().find((entry) => entry.file === name)?.recipe as { seconds: number } | undefined
  const answers: Record<string, unknown> = {
    list_looms: [{ id: loom.id, name: loom.name, updatedAt: loom.updatedAt, nodes: loom.nodes.length }],
    read_loom: loom,
    save_loom: { id: loom.id, name, updatedAt: Date.now(), nodes: loom.nodes.length },
    list_generated_songs: fixtureSongs(),
    list_generated_samples: FIXTURE_SAMPLES,
    library_clip_peaks: recipe && { min: [], max: [], duration: recipe.seconds },
  }
  const answer = answers[cmd]
  return answer ? Promise.resolve(answer as T) : Promise.reject(new Error('Tauri IPC unavailable'))
}
