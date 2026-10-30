import { act, render } from '@testing-library/react'
import { Profiler, useRef } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { BeatClock } from '../deck/useDeck'
import { LoomScope, type LoomScopeProps } from './LoomScope'
import { useBeatPulse } from './visuals'

// jsdom has no 2D canvas and no real frame clock: a fake context records the
// drawing, and frames run only when a test flushes one.
let frames: Map<number, FrameRequestCallback>
let nextFrame: number
let calls: string[]

function flushFrame(timestamp: number) {
  const due = [...frames.values()]
  frames.clear()
  act(() => {
    for (const callback of due) callback(timestamp)
  })
}

beforeEach(() => {
  frames = new Map()
  nextFrame = 1
  calls = []
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    frames.set(nextFrame, callback)
    return nextFrame++
  })
  vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id))
  const context = new Proxy(
    {},
    {
      get: (_target, name) => (...args: unknown[]) => calls.push(`${String(name)}(${args.length})`),
      set: (_target, name, value) => {
        calls.push(`${String(name)}=${String(value)}`)
        return true
      },
    },
  )
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(
    context as unknown as CanvasRenderingContext2D,
  )
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

const CLOCK: BeatClock = { periodSeconds: 0.5, beatAtContext: 10 }

function scopeProps(levels: { a: number; b: number }): LoomScopeProps {
  return {
    crossfade: 0.5,
    decks: {
      a: { getLevel: () => levels.a, getBeat: () => CLOCK },
      b: { getLevel: () => levels.b, getBeat: () => null },
    },
  }
}

describe('LoomScope', () => {
  it('draws one fade and a figure per sounding deck each frame, without re-rendering', () => {
    let commits = 0
    render(
      <Profiler id="scope" onRender={() => commits++}>
        <LoomScope {...scopeProps({ a: 0.6, b: 0.3 })} />
      </Profiler>,
    )
    const mounted = commits
    flushFrame(0)

    expect(calls).toContain('globalCompositeOperation=destination-out')
    expect(calls.filter((call) => call === 'fillRect(4)')).toHaveLength(1)
    expect(calls.filter((call) => call === 'stroke(0)')).toHaveLength(2)
    expect(frames.size).toBe(1)
    expect(commits).toBe(mounted)
  })

  it('leaves a silent deck out of the figure', () => {
    render(<LoomScope {...scopeProps({ a: 0.6, b: 0 })} />)
    flushFrame(0)

    expect(calls.filter((call) => call === 'stroke(0)')).toHaveLength(1)
  })

  it('skips drawing while the page is hidden', () => {
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden')
    render(<LoomScope {...scopeProps({ a: 0.6, b: 0.3 })} />)
    flushFrame(0)

    expect(calls).not.toContain('fillRect(4)')
    expect(frames.size).toBe(1)
  })

  it('cancels its frame on unmount', () => {
    const { unmount } = render(<LoomScope {...scopeProps({ a: 0.6, b: 0.3 })} />)
    flushFrame(0)
    unmount()

    expect(frames.size).toBe(0)
  })
})

const noBeat = () => null
const contextTime = () => 10.25

function Booth(props: { enabled: boolean; getBeatA: () => BeatClock | null }) {
  const root = useRef<HTMLElement>(null)
  useBeatPulse(root, props.enabled, props.getBeatA, noBeat, contextTime)
  return <section ref={root} data-testid="booth" />
}

describe('useBeatPulse', () => {
  it('writes each deck’s beat phase onto the root as a CSS variable, without re-rendering', () => {
    let commits = 0
    const { getByTestId } = render(
      <Profiler id="booth" onRender={() => commits++}>
        <Booth enabled getBeatA={() => CLOCK} />
      </Profiler>,
    )
    const mounted = commits
    flushFrame(0)
    const booth = getByTestId('booth')

    expect(booth.style.getPropertyValue('--loom-beat-a')).toBe('0.500')
    expect(booth.style.getPropertyValue('--loom-beat-b')).toBe('')
    expect(commits).toBe(mounted)
  })

  it('runs no frame loop while visuals are off', () => {
    render(<Booth enabled={false} getBeatA={() => CLOCK} />)

    expect(frames.size).toBe(0)
  })

  it('clears its variables and its frame on unmount', () => {
    const { getByTestId, unmount } = render(<Booth enabled getBeatA={() => CLOCK} />)
    flushFrame(0)
    const booth = getByTestId('booth')
    unmount()

    expect(booth.style.getPropertyValue('--loom-beat-a')).toBe('')
    expect(frames.size).toBe(0)
  })
})
