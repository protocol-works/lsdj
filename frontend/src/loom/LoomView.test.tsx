import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { seedState } from './fixture'
import { loomReducer, type LoomAction, type LoomState } from './loomState'
import { LoomView, type LoomViewProps } from './LoomView'
import { barStartSeconds } from './timing'

const PROMPTS = {
  r1: 'ambient dub, tape delay, submerged',
  n2: 'dub techno, rolling bassline',
  n4: 'acid techno, 303 squelch, peak time',
  n5: 'deep house, rain on glass, rhodes',
  n7: 'breakbeat jungle, chopped amens, reese bass',
}

function makeProps(overrides: Partial<LoomViewProps> = {}): LoomViewProps {
  return {
    state: seedState(),
    pxPerBar: 20,
    onPxPerBar: vi.fn(),
    scrollX: 0,
    onScrollX: vi.fn(),
    deckStatus: { a: null, b: null },
    peaks: vi.fn(async () => ({ min: [-0.5, -0.2, -0.4], max: [0.5, 0.3, 0.6], duration: 40 })),
    progress: {},
    onTie: vi.fn(),
    onUntie: vi.fn(),
    onCut: vi.fn(),
    onCue: vi.fn(),
    onScrub: vi.fn(),
    onTrim: vi.fn(),
    onKeep: vi.fn(),
    onDiscard: vi.fn(),
    onRetry: vi.fn(),
    onWeave: vi.fn(),
    onSelect: vi.fn(),
    renderAhead: true,
    ...overrides,
  }
}

function renderView(overrides: Partial<LoomViewProps> = {}) {
  const props = makeProps(overrides)
  const view = render(<LoomView {...props} />)
  return { props, ...view }
}

const nodeEl = (prompt: string) => screen.getByRole('group', { name: prompt })
const reduce = (state: LoomState, ...actions: LoomAction[]) => actions.reduce(loomReducer, state)
const withDeckA = (patch: Partial<LoomState['decks']['a']>): LoomState => {
  const state = seedState()
  return { ...state, decks: { ...state.decks, a: { ...state.decks.a, ...patch } } }
}
/** Three takes after n4: g1 rendered, g2 rendering, g3 queued. */
const takesState = () =>
  reduce(
    seedState(),
    {
      type: 'weave',
      parentId: 'n4',
      prompt: 'acid',
      takes: 3,
      bars: 16,
      bpm: 124,
      loras: [],
      sa3: {},
      seeds: [1, 2, 3],
      ids: ['g1', 'g2', 'g3'],
      takeGroup: 't1',
      auto: false,
      now: 0,
    },
    { type: 'rendered', id: 'g1', file: 'g1.wav', durationSeconds: 38.7 },
  )

describe('LoomView canvas', () => {
  it('renders one node per fixture entry with children right of their parents', async () => {
    const { container, props } = renderView()
    expect(container.querySelectorAll('.loom-node')).toHaveLength(seedState().loom.nodes.length)
    const left = (prompt: string) => parseFloat(nodeEl(prompt).style.left)
    const width = (prompt: string) => parseFloat(nodeEl(prompt).style.width)
    expect(left(PROMPTS.n2)).toBeGreaterThan(left(PROMPTS.r1) + width(PROMPTS.r1))
    expect(left(PROMPTS.n5)).toBeGreaterThan(left(PROMPTS.n2) + width(PROMPTS.n2))
    // 16 bars at 20 px/bar is 320 px of clip: one bucket per pixel, rounded up.
    expect(props.peaks).toHaveBeenCalledWith('r1.wav', 512)
    await waitFor(() => expect(nodeEl(PROMPTS.r1).querySelector('.loom-node__peaks')).not.toBeNull())
  })

  it('fetches a node envelope once per file and bucket count', () => {
    const { props, rerender } = renderView()
    rerender(<LoomView {...props} pxPerBar={21} state={{ ...props.state, selected: 'n2' }} />)
    expect(vi.mocked(props.peaks).mock.calls.filter(([file]) => file === 'r1.wav')).toHaveLength(1)
  })

  it('shows the empty state with a weave button for a root', () => {
    const state = seedState()
    renderView({ state: { ...state, loom: { ...state.loom, nodes: [] } } })
    expect(screen.getByText('Weave a thread to begin: drop a song here, or press +.')).toBeInTheDocument()
    fireEvent.click(screen.getAllByRole('button', { name: 'Weave a new thread' })[0])
    expect(screen.getByRole('dialog', { name: 'Weave' })).toBeInTheDocument()
  })

  it('drags a node as a loom: payload', () => {
    renderView()
    const setData = vi.fn()
    fireEvent.dragStart(nodeEl(PROMPTS.n2), { dataTransfer: { setData, effectAllowed: '' } })
    expect(setData).toHaveBeenCalledWith('text/plain', 'loom:n2')
  })
})

describe('LoomView node tools', () => {
  it('ties with →A and cuts with shift-click', () => {
    const { props } = renderView()
    const tie = within(nodeEl(PROMPTS.n2)).getByRole('button', { name: '→ A' })
    fireEvent.click(tie)
    expect(props.onTie).toHaveBeenCalledWith('a', 'n2')
    fireEvent.click(tie, { shiftKey: true })
    expect(props.onCut).toHaveBeenCalledWith('a', 'n2')
  })

  it('unties a node the deck has queued', () => {
    const { props } = renderView({ state: withDeckA({ nodeId: 'n2', queue: ['n7'] }) })
    fireEvent.click(within(nodeEl(PROMPTS.n7)).getByRole('button', { name: '⨯ A' }))
    expect(props.onUntie).toHaveBeenCalledWith('a', 'n7')
  })

  it('cues from the join with ⌾ and stops a running cue with ⌾ or Escape', () => {
    const first = renderView()
    fireEvent.click(within(nodeEl(PROMPTS.n5)).getByRole('button', { name: '⌾ cue' }))
    expect(first.props.onCue).toHaveBeenCalledWith('n5', null)
    first.unmount()

    const { props } = renderView({ state: withDeckA({ cue: { nodeId: 'n5', fromBar: -4 } }) })
    expect(within(nodeEl(PROMPTS.n5)).getByText('⌾ cue · from the join (−4 bars)')).toBeInTheDocument()
    fireEvent.click(within(nodeEl(PROMPTS.n5)).getByRole('button', { name: '⌾ cue' }))
    expect(props.onCue).toHaveBeenLastCalledWith(null, null)
    vi.mocked(props.onCue).mockClear()
    fireEvent.keyDown(document.body, { key: 'Escape' })
    expect(props.onCue).toHaveBeenCalledWith(null, null)
  })

  it('scrubs the deck on its own node and cues anywhere else, from the clicked bar', () => {
    const state = withDeckA({ nodeId: 'n2' })
    const { props } = renderView({ state })
    const clickWave = (prompt: string, clientX: number) => {
      const wave = nodeEl(prompt).querySelector('.loom-node__wave') as SVGSVGElement
      vi.spyOn(wave, 'getBoundingClientRect').mockReturnValue(new DOMRect(0, 0, 160, 24))
      fireEvent.click(wave, { clientX })
    }
    // 85 of 160 px into a 16-bar body is bar 8.
    clickWave(PROMPTS.n2, 85)
    const n2 = state.loom.nodes.find((n) => n.id === 'n2')!
    expect(props.onScrub).toHaveBeenCalledWith('a', barStartSeconds(n2, 8))
    expect(props.onCue).not.toHaveBeenCalled()
    clickWave(PROMPTS.n5, 85)
    expect(props.onCue).toHaveBeenCalledWith('n5', 8)
  })

  it('trims to whole bars and never below four', () => {
    const { props } = renderView()
    const node = nodeEl(PROMPTS.n4)
    const drag = (handle: Element, from: number, to: number) => {
      fireEvent.pointerDown(handle, { pointerId: 1, clientX: from })
      fireEvent.pointerMove(handle, { pointerId: 1, clientX: to })
      fireEvent.pointerUp(handle, { pointerId: 1, clientX: to })
    }
    drag(node.querySelector('.loom-node__handle--out')!, 300, 0)
    expect(props.onTrim).toHaveBeenLastCalledWith('n4', 0, 4)
    // 68 px at 20 px/bar rounds to three bars.
    drag(node.querySelector('.loom-node__handle--in')!, 100, 168)
    expect(props.onTrim).toHaveBeenLastCalledWith('n4', 3, 16)
    // A parent's out-point is where its children start.
    expect(nodeEl(PROMPTS.r1).querySelector('.loom-node__handle--out')).toBeNull()
  })

  it('shows a rendering node with its ETA and only a discard tool', () => {
    const { props, container } = renderView({ state: takesState(), progress: { g2: { elapsed: 2, eta: 8 } } })
    const [rendering, queued] = container.querySelectorAll<HTMLElement>('.loom-node--ghost')
    expect(within(rendering).getByText('⟳ 0:06')).toBeInTheDocument()
    expect(within(queued).getByText(/^queued · /)).toBeInTheDocument()
    expect(within(rendering).getAllByRole('button').map((b) => b.textContent)).toEqual(['⨯'])
    fireEvent.click(within(rendering).getByRole('button', { name: 'Discard' }))
    expect(props.onDiscard).toHaveBeenCalledWith('g2')
  })

  it('brackets a take group with how many have rendered', () => {
    renderView({ state: takesState() })
    expect(screen.getByText('3 takes · 1/3 rendered')).toBeInTheDocument()
  })

  it('keeps a rendered take', () => {
    const { props, container } = renderView({ state: takesState() })
    const take = container.querySelector<HTMLElement>('.loom-node--take:not(.loom-node--ghost)')!
    expect(within(take).getByText(/take 1/)).toBeInTheDocument()
    fireEvent.click(within(take).getByRole('button', { name: '✓ keep' }))
    expect(props.onKeep).toHaveBeenCalledWith('g1')
  })

  it('shows why a render failed and offers retry', () => {
    const state = reduce(takesState(), { type: 'render_failed', id: 'g2', error: 'sidecar down' })
    const { props } = renderView({ state })
    const failed = screen.getByText('failed · sidecar down').closest('.loom-node') as HTMLElement
    fireEvent.click(within(failed).getByRole('button', { name: '⟳ retry' }))
    expect(props.onRetry).toHaveBeenCalledWith('g2')
  })
})

describe('LoomView shuttles', () => {
  it('counts down to the tied node', () => {
    const state = withDeckA({ nodeId: 'n2', queue: ['n7'] })
    const n2 = state.loom.nodes.find((n) => n.id === 'n2')!
    const { container } = renderView({
      state,
      deckStatus: { a: { position: barStartSeconds(n2, 4), playing: true, loop: null }, b: null },
    })
    const tag = container.querySelector('.loom__tag')!
    expect(tag).toHaveTextContent(`A · 12 → ${PROMPTS.n7}`)
    expect(tag).not.toHaveClass('loom__tag--warn')
  })

  it('warns of the hold within eight bars when nothing is tied', () => {
    const state = withDeckA({ nodeId: 'n7' })
    const n7 = state.loom.nodes.find((n) => n.id === 'n7')!
    const { container } = renderView({
      state,
      deckStatus: { a: { position: barStartSeconds(n7, 10), playing: true, loop: null }, b: null },
    })
    expect(container.querySelector('.loom__tag')).toHaveTextContent('A · 6 → HOLD')
    expect(container.querySelector('.loom__tag')).toHaveClass('loom__tag--warn')
    expect(container.querySelector('.loom__loop--hold')).not.toBeNull()
    expect(screen.getByRole('button', { name: '+ A is holding — what next?' })).toBeInTheDocument()
  })
})

describe('LoomView zoom', () => {
  it('zooms with - = 0 inside 6–48 px per bar', () => {
    const { props, rerender } = renderView()
    fireEvent.keyDown(document.body, { key: '=' })
    expect(props.onPxPerBar).toHaveBeenLastCalledWith(25)
    fireEvent.keyDown(document.body, { key: '-' })
    expect(props.onPxPerBar).toHaveBeenLastCalledWith(16)
    rerender(<LoomView {...props} pxPerBar={46} />)
    fireEvent.keyDown(document.body, { key: '=' })
    expect(props.onPxPerBar).toHaveBeenLastCalledWith(48)
    rerender(<LoomView {...props} pxPerBar={7} />)
    fireEvent.keyDown(document.body, { key: '-' })
    expect(props.onPxPerBar).toHaveBeenLastCalledWith(6)
    fireEvent.keyDown(document.body, { key: '0' })
    expect(props.onPxPerBar).toHaveBeenLastCalledWith(20)
  })

  it('ignores the zoom keys while typing in the weave popover', () => {
    const { props } = renderView()
    fireEvent.click(within(nodeEl(PROMPTS.n2)).getByRole('button', { name: 'Weave what comes next' }))
    const prompt = within(screen.getByRole('dialog', { name: 'Weave' })).getByRole('textbox', { name: 'Prompt' })
    fireEvent.keyDown(prompt, { key: '-' })
    fireEvent.keyDown(prompt, { key: '=' })
    fireEvent.keyDown(prompt, { key: '0' })
    expect(props.onPxPerBar).not.toHaveBeenCalled()
  })

  it('leaves 0 to the open explorer (it clears a rating) but keeps - and =', () => {
    const { props } = renderView({ pxPerBar: 30, explorerOpen: true })
    fireEvent.keyDown(document.body, { key: '0' })
    expect(props.onPxPerBar).not.toHaveBeenCalled()
    fireEvent.keyDown(document.body, { key: '-' })
    expect(props.onPxPerBar).toHaveBeenLastCalledWith(24)
  })
})

describe('WeavePopover', () => {
  it('weaves after a node with the form, the blank prompt continuing the parent', () => {
    const { props } = renderView({ state: withDeckA({ nodeId: 'n2' }) })
    fireEvent.click(within(nodeEl(PROMPTS.n2)).getByRole('button', { name: 'Weave what comes next' }))
    const dialog = within(screen.getByRole('dialog', { name: 'Weave' }))
    fireEvent.click(within(dialog.getByRole('radiogroup', { name: 'Takes' })).getByRole('radio', { name: '2' }))
    fireEvent.click(within(dialog.getByRole('radiogroup', { name: 'Bars' })).getByRole('radio', { name: '32' }))
    fireEvent.click(dialog.getByRole('button', { name: 'Weave & tie A' }))
    expect(props.onWeave).toHaveBeenCalledWith('n2', {
      prompt: PROMPTS.n2,
      takes: 2,
      bars: 32,
      bpm: 124,
      loras: [],
      sa3: {},
      seed: null,
      tieTo: 'a',
    })
    expect(screen.queryByRole('dialog', { name: 'Weave' })).toBeNull()
  })

  it('weaves a root from a prompt at its tempo with guidance on', () => {
    const { props } = renderView()
    fireEvent.click(screen.getByRole('button', { name: '+ thread' }))
    const dialog = within(screen.getByRole('dialog', { name: 'Weave' }))
    expect(dialog.queryByRole('button', { name: /tie/ })).toBeNull()
    fireEvent.change(dialog.getByRole('textbox', { name: 'Prompt' }), { target: { value: 'dub siren' } })
    fireEvent.change(dialog.getByRole('spinbutton', { name: 'Tempo (BPM)' }), { target: { value: '128' } })
    fireEvent.click(dialog.getByRole('switch', { name: 'Guidance: Off' }))
    fireEvent.click(dialog.getByRole('button', { name: 'Weave ×3' }))
    expect(props.onWeave).toHaveBeenCalledWith(null, {
      prompt: 'dub siren',
      takes: 3,
      bars: 16,
      bpm: 128,
      loras: [],
      sa3: { cfg: 3, apg: 1 },
      seed: null,
      tieTo: null,
    })
  })

  it('refuses a root without a prompt', () => {
    renderView()
    fireEvent.click(screen.getByRole('button', { name: '+ thread' }))
    expect(within(screen.getByRole('dialog', { name: 'Weave' })).getByRole('button', { name: 'Weave ×3' })).toBeDisabled()
  })

  it('says when the render will be late for the on-air seam', () => {
    const state = withDeckA({ nodeId: 'n2' })
    const n2 = state.loom.nodes.find((n) => n.id === 'n2')!
    renderView({ state, deckStatus: { a: { position: barStartSeconds(n2, 15), playing: true, loop: null }, b: null } })
    fireEvent.click(within(nodeEl(PROMPTS.n2)).getByRole('button', { name: 'Weave what comes next' }))
    expect(screen.getByText(/A reaches this seam in 0:02 → late by 0:\d\d, A will hold/)).toBeInTheDocument()
  })
})
