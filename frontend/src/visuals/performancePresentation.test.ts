/// <reference types="node" />

import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'

const appCss = readFileSync('src/index.css', 'utf8')

function ruleBody(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const matches = [
    ...css.matchAll(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`, 'g')),
  ]
  const match = matches.at(-1) ?? null
  expect(match, `missing CSS rule for ${selector}`).not.toBeNull()
  return match?.[1] ?? ''
}

describe('performance visual presentation contract', () => {
  it('paints a panel-coloured stage behind translucent booth modules', () => {
    const layer = ruleBody(appCss, '.performance-visuals')

    expect(layer).toContain('z-index: -1')
    expect(layer).toContain('pointer-events: none')
    expect(layer).toContain('overflow: hidden')
    expect(layer).toContain('var(--color-surface)')
    expect(appCss).toContain(".app[data-performance-visuals='on'] .deck")
    expect(appCss).toContain('var(--color-surface) 76%')
  })

  it('uses stronger gain-and-beat glows from the live deck tokens', () => {
    expect(appCss).toContain('var(--color-deck-a)')
    expect(appCss).toContain('var(--performance-wash-a) * 0.46')
    expect(appCss).toContain('var(--performance-pulse-a) * 0.68')
    expect(appCss).toContain('0.82')
  })

  it('keeps one fixed-size plus pattern responsive to output energy', () => {
    const grid = ruleBody(appCss, '.performance-visuals__grid')

    expect(grid).toContain('radial-gradient')
    expect(grid).toContain('var(--performance-energy)')
    expect(grid).toContain('ellipse 1px 5px')
    expect(grid).not.toContain('var(--performance-pulse-a)')
    expect(appCss).not.toContain('.performance-visuals__grid-hit')
  })
})
