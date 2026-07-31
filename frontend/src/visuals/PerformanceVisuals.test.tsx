import { act, render } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RefObject } from 'react'

import {
  PerformanceVisuals,
  type PerformanceVisualsProps,
} from './PerformanceVisuals'

const CLOCK = { periodSeconds: 0.5, beatAtContext: 10 }
const PERFORMANCE_VISUAL_PROPERTIES = [
  '--performance-wash-a',
  '--performance-wash-b',
  '--performance-pulse-a',
  '--performance-pulse-b',
]

let nextFrameId: number
let frames: Map<number, FrameRequestCallback>
let requestFrame: ReturnType<typeof vi.fn>
let cancelFrame: ReturnType<typeof vi.fn>
let reducedMotion: boolean
let motionListeners: Set<() => void>

function flushFrame(timestamp: number) {
  const due = [...frames.values()]
  frames.clear()
  act(() => {
    for (const callback of due) callback(timestamp)
  })
}

function setReducedMotion(reduced: boolean) {
  reducedMotion = reduced
  act(() => {
    for (const listener of motionListeners) listener()
  })
}

function createRootRef() {
  const root = document.createElement('main')
  return {
    root,
    rootRef: { current: root } as RefObject<HTMLElement | null>,
  }
}

function createProps(
  rootRef: RefObject<HTMLElement | null>,
  overrides: Partial<PerformanceVisualsProps> = {},
): PerformanceVisualsProps {
  return {
    enabled: true,
    rootRef,
    crossfade: 0.5,
    getContextTime: vi.fn(() => 10),
    getMasterLevel: vi.fn(() => 1),
    decks: {
      a: {
        audible: true,
        getLevel: vi.fn(() => 1),
        getBeat: vi.fn(() => CLOCK),
      },
      b: {
        audible: true,
        getLevel: vi.fn(() => 1),
        getBeat: vi.fn(() => CLOCK),
      },
    },
    ...overrides,
  }
}

function visualValue(root: HTMLElement, property: string): number {
  return Number(root.style.getPropertyValue(property))
}

beforeEach(() => {
  nextFrameId = 1
  frames = new Map()
  reducedMotion = false
  motionListeners = new Set()
  requestFrame = vi.fn((callback: FrameRequestCallback) => {
    const id = nextFrameId++
    frames.set(id, callback)
    return id
  })
  cancelFrame = vi.fn((id: number) => {
    frames.delete(id)
  })
  vi.stubGlobal('requestAnimationFrame', requestFrame)
  vi.stubGlobal('cancelAnimationFrame', cancelFrame)
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => ({
      get matches() {
        return reducedMotion
      },
      media: '(prefers-reduced-motion: reduce)',
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: (_type: string, listener: () => void) => {
        motionListeners.add(listener)
      },
      removeEventListener: (_type: string, listener: () => void) => {
        motionListeners.delete(listener)
      },
      dispatchEvent: vi.fn(() => true),
    })),
  )
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('PerformanceVisuals', () => {
  it('uses one RAF for both decks and reads every source at most once per frame', () => {
    const { root, rootRef } = createRootRef()
    const props = createProps(rootRef)
    const { container } = render(<PerformanceVisuals {...props} />)
    const initialDom = container.innerHTML

    expect(frames.size).toBe(1)
    expect(container.querySelector('.performance-visuals__grid')).toBeInTheDocument()
    expect(container.querySelector('.performance-visuals__grid-hit')).toBeNull()
    flushFrame(0)
    expect(frames.size).toBe(1)
    expect(props.getContextTime).toHaveBeenCalledTimes(1)
    expect(props.getMasterLevel).toHaveBeenCalledTimes(1)
    expect(props.decks.a.getLevel).toHaveBeenCalledTimes(1)
    expect(props.decks.b.getLevel).toHaveBeenCalledTimes(1)
    expect(props.decks.a.getBeat).toHaveBeenCalledTimes(1)
    expect(props.decks.b.getBeat).toHaveBeenCalledTimes(1)

    flushFrame(1000 / 60)
    expect(visualValue(root, '--performance-wash-a')).toBeGreaterThan(0)
    expect(visualValue(root, '--performance-wash-b')).toBeGreaterThan(0)
    expect(visualValue(root, '--performance-pulse-a')).toBeGreaterThan(0)
    expect(container.innerHTML).toBe(initialDom)
  })

  it('reads current props without restarting the loop', () => {
    const { root, rootRef } = createRootRef()
    const props = createProps(rootRef)
    const view = render(<PerformanceVisuals {...props} />)
    flushFrame(0)
    flushFrame(1000 / 60)
    const requestsBeforeUpdate = requestFrame.mock.calls.length

    view.rerender(<PerformanceVisuals {...props} crossfade={0} />)
    expect(requestFrame).toHaveBeenCalledTimes(requestsBeforeUpdate)
    expect(frames.size).toBe(1)
    flushFrame(2000 / 60)

    expect(root.style.getPropertyValue('--performance-wash-b')).toBe('0.0000')
    expect(visualValue(root, '--performance-wash-a')).toBeGreaterThan(0)
  })

  it('hard-gates inaudible decks and contains failing or invalid getters', () => {
    const { root, rootRef } = createRootRef()
    const props = createProps(rootRef, {
      getContextTime: vi.fn(() => {
        throw new Error('snapshot unavailable')
      }),
      getMasterLevel: vi.fn(() => Number.NaN),
      decks: {
        a: {
          audible: false,
          getLevel: vi.fn(() => 1),
          getBeat: vi.fn(() => CLOCK),
        },
        b: {
          audible: true,
          getLevel: vi.fn(() => {
            throw new Error('level unavailable')
          }),
          getBeat: vi.fn(() => CLOCK),
        },
      },
    })
    render(<PerformanceVisuals {...props} />)

    expect(() => {
      flushFrame(0)
      flushFrame(1000 / 60)
    }).not.toThrow()
    expect(props.decks.a.getBeat).not.toHaveBeenCalled()
    for (const property of PERFORMANCE_VISUAL_PROPERTIES) {
      expect(root.style.getPropertyValue(property)).toBe('0.0000')
    }
  })

  it('skips identical CSS strings', () => {
    const { root, rootRef } = createRootRef()
    const setProperty = vi.spyOn(root.style, 'setProperty')
    render(<PerformanceVisuals {...createProps(rootRef)} />)

    flushFrame(0)
    expect(setProperty).toHaveBeenCalledTimes(4)
    flushFrame(0)
    expect(setProperty).toHaveBeenCalledTimes(4)
  })

  it('uses a static half-strength wash and no RAF under reduced motion', () => {
    reducedMotion = true
    const { root, rootRef } = createRootRef()
    const props = createProps(rootRef)
    const view = render(<PerformanceVisuals {...props} />)

    expect(frames.size).toBe(0)
    expect(root.style.getPropertyValue('--performance-wash-a')).toBe('0.2500')
    expect(root.style.getPropertyValue('--performance-wash-b')).toBe('0.2500')
    expect(root.style.getPropertyValue('--performance-pulse-a')).toBe('0.0000')
    expect(props.getMasterLevel).not.toHaveBeenCalled()

    view.rerender(<PerformanceVisuals {...props} crossfade={0} />)
    expect(root.style.getPropertyValue('--performance-wash-a')).toBe('0.5000')
    expect(root.style.getPropertyValue('--performance-wash-b')).toBe('0.0000')
  })

  it('stops and resumes the loop when reduced motion changes live', () => {
    const { root, rootRef } = createRootRef()
    render(<PerformanceVisuals {...createProps(rootRef)} />)
    expect(frames.size).toBe(1)

    setReducedMotion(true)
    expect(frames.size).toBe(0)
    expect(cancelFrame).toHaveBeenCalledTimes(1)
    expect(root.style.getPropertyValue('--performance-pulse-a')).toBe('0.0000')

    setReducedMotion(false)
    expect(frames.size).toBe(1)
  })

  it('fully stops, clears, and restarts when toggled', () => {
    const { root, rootRef } = createRootRef()
    const props = createProps(rootRef)
    const view = render(<PerformanceVisuals {...props} enabled={false} />)
    const layer = view.container.querySelector('.performance-visuals')

    expect(layer).toHaveAttribute('aria-hidden', 'true')
    expect(layer).toHaveAttribute('hidden')
    expect(frames.size).toBe(0)
    expect(motionListeners.size).toBe(0)

    view.rerender(<PerformanceVisuals {...props} />)
    expect(frames.size).toBe(1)
    expect(motionListeners.size).toBe(1)
    view.rerender(<PerformanceVisuals {...props} enabled={false} />)
    expect(frames.size).toBe(0)
    expect(motionListeners.size).toBe(0)
    for (const property of PERFORMANCE_VISUAL_PROPERTIES) {
      expect(root.style.getPropertyValue(property)).toBe('')
    }
  })

  it('is inert when browser animation and preference APIs are absent', () => {
    vi.stubGlobal('requestAnimationFrame', undefined)
    vi.stubGlobal('matchMedia', undefined)
    const { root, rootRef } = createRootRef()

    expect(() => render(<PerformanceVisuals {...createProps(rootRef)} />)).not.toThrow()
    expect(root.getAttribute('style')).toBeNull()
  })

  it('cancels the frame, clears styles, and leaves no interactive content on unmount', () => {
    const { root, rootRef } = createRootRef()
    const view = render(<PerformanceVisuals {...createProps(rootRef)} />)
    flushFrame(0)

    expect(view.container.querySelectorAll('button, a, input, [tabindex]')).toHaveLength(0)
    view.unmount()
    expect(frames.size).toBe(0)
    expect(cancelFrame).toHaveBeenCalled()
    for (const property of PERFORMANCE_VISUAL_PROPERTIES) {
      expect(root.style.getPropertyValue(property)).toBe('')
    }
  })
})
