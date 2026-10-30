/** Test fixture: the loom mockup's seed graph (`mockups/loom/index.html`),
 * every node rendered with a clip of exactly its kept tail plus its bars. */

import type { LoraChoice } from '../models/useLoras'
import { initialLoomState, type Loom, type LoomNode, type LoomState } from './loomState'
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
