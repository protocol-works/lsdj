import { useEffect, useRef, useState, type RefObject } from 'react'
import { useTranslation } from 'react-i18next'

import type { DeckId } from '../audio/types'
import { Sa3AdvancedControls } from '../generation/Sa3AdvancedControls'
import { DEFAULT_SA3_STEERING, parseFixedSeed, type Sa3SteeringDraft } from '../generation/songGeneration'
import { stackForKind, useLoras, useLoraStack, type LoraChoice } from '../models/useLoras'
import { AnchoredPanel } from '../ui/AnchoredPanel'
import { Button } from '../ui/Button'
import { LoraControl } from '../ui/LoraControl'
import { SegmentedControl } from '../ui/SegmentedControl'
import { TextField } from '../ui/TextField'
import type { LoomNode } from './loomState'
import { barSeconds, DEFAULT_BARS, DEFAULT_BPM, fmtClock, lateBy, RENDER, renderEta, TAIL_BARS } from './timing'

/** What a weave asks for. `prompt` is already resolved (blank = the
 * parent's); `seed` null = a random seed per take, else the first take's. */
export type WeaveForm = {
  prompt: string
  takes: number
  bars: number
  bpm: number
  loras: LoraChoice[]
  sa3: LoomNode['sa3']
  seed: number | null
  tieTo: DeckId | null
}

/** A deck on the parent: it can be tied at once, and when it plays the
 * popover says whether the render beats it to the seam. */
export type WeaveDeck = { deck: DeckId; secondsToSeam: number | null }

const TAKES = ['1', '2', '3', '4'].map((value) => ({ value, label: value }))
const BARS = ['8', '16', '32'].map((value) => ({ value, label: value }))
const TEMPO_MIN = 40
const TEMPO_MAX = 300

/** The weave form, anchored to the button that opened it: prompt, takes,
 * bars (and tempo for a root), the LoRA stack, the SA3 knobs behind a
 * disclosure, and the render-time line. Stays mounted so takes, bars and the
 * stack carry over from one weave to the next. */
export function WeavePopover({
  open,
  anchorRef,
  parent,
  decks,
  onWeave,
  onClose,
}: {
  open: boolean
  anchorRef: RefObject<HTMLElement | null>
  parent: LoomNode | null
  decks: WeaveDeck[]
  onWeave: (form: WeaveForm) => void
  onClose: () => void
}) {
  const { t } = useTranslation()
  const loras = useLoras()
  const stack = useLoraStack()
  const [prompt, setPrompt] = useState('')
  const [takes, setTakes] = useState(3)
  const [bars, setBars] = useState(DEFAULT_BARS)
  const [tempo, setTempo] = useState(String(DEFAULT_BPM))
  const [steering, setSteering] = useState<Sa3SteeringDraft>(DEFAULT_SA3_STEERING)
  const bodyRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (open) bodyRef.current?.querySelector('input')?.focus()
  }, [open])

  const bpm = parent ? parent.bpm : Number(tempo)
  const tempoValid = Number.isFinite(bpm) && bpm >= TEMPO_MIN && bpm <= TEMPO_MAX
  const resolvedPrompt = prompt.trim() || parent?.prompt || ''
  const seedValid = steering.seed.mode === 'random' || parseFixedSeed(steering.seed.value) !== null
  const valid = resolvedPrompt !== '' && tempoValid && seedValid
  const requestLoras = stackForKind(stack.stack, loras, 'track')

  const eta = renderEta({ bars, tailBars: parent ? TAIL_BARS : 0, bpm: tempoValid ? bpm : DEFAULT_BPM, loras: requestLoras })
  const seconds = Math.round(bars * barSeconds(tempoValid ? bpm : DEFAULT_BPM))
  const etaLine = [
    takes > 1
      ? t('loom.weave.etaTakes', { seconds, bpm: tempoValid ? bpm : DEFAULT_BPM, time: fmtClock(eta), total: fmtClock(eta * takes), takes })
      : t('loom.weave.eta', { seconds, bpm: tempoValid ? bpm : DEFAULT_BPM, time: fmtClock(eta) }),
    ...decks.flatMap(({ deck, secondsToSeam }) => {
      if (secondsToSeam === null) return []
      const letter = t(deck === 'a' ? 'mixer.deckA' : 'mixer.deckB')
      const late = lateBy(eta, secondsToSeam)
      return [
        late > 0
          ? t('loom.weave.late', { deck: letter, time: fmtClock(secondsToSeam), late: fmtClock(late) })
          : t('loom.weave.inTime', { deck: letter, time: fmtClock(secondsToSeam) }),
      ]
    }),
  ].join(' · ')
  const late = decks.some(({ secondsToSeam }) => secondsToSeam !== null && lateBy(eta, secondsToSeam) > 0)

  const submit = (tieTo: DeckId | null) => {
    if (!valid) return
    const guided = steering.guidance === 'on'
    onWeave({
      prompt: resolvedPrompt,
      takes,
      bars,
      bpm,
      loras: requestLoras,
      sa3: guided
        ? { cfg: steering.cfg, apg: steering.apg, negativePrompt: steering.negativePrompt.trim() || undefined }
        : {},
      seed: steering.seed.mode === 'fixed' ? parseFixedSeed(steering.seed.value) : null,
      tieTo,
    })
    setPrompt('')
    onClose()
  }

  return (
    <AnchoredPanel
      open={open}
      anchorRef={anchorRef}
      onClose={onClose}
      label={t('loom.weave.label')}
      className="loom-weave"
    >
      <div ref={bodyRef} className="loom-weave__body">
        <h4 className="loom-weave__title">
          {parent ? t('loom.weave.after', { prompt: parent.prompt }) : t('loom.weave.root')}
        </h4>
        <TextField
          label={t('loom.weave.prompt')}
          placeholder={t(parent ? 'loom.weave.continuePlaceholder' : 'loom.weave.rootPlaceholder')}
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter') submit(null)
          }}
        />
        <div className="loom-weave__row">
          <SegmentedControl label={t('loom.weave.takes')} value={String(takes)} options={TAKES} onChange={(value) => setTakes(Number(value))} />
          <SegmentedControl label={t('loom.weave.bars')} value={String(bars)} options={BARS} onChange={(value) => setBars(Number(value))} />
          {parent ? null : (
            <TextField
              label={t('loom.weave.tempo')}
              type="number"
              inputMode="decimal"
              min={TEMPO_MIN}
              max={TEMPO_MAX}
              aria-invalid={!tempoValid}
              value={tempo}
              onChange={(event) => setTempo(event.target.value)}
            />
          )}
        </div>
        <LoraControl
          adapters={loras}
          kind="track"
          value={stack.stack}
          onToggle={stack.toggle}
          onStrength={stack.setStrength}
          onToggleBypass={stack.toggleBypass}
        />
        <p className={`loom-weave__eta${late ? ' loom-weave__eta--warn' : ''}`}>{etaLine}</p>
        {requestLoras.length > 0 ? (
          <p className="loom-weave__eta loom-weave__eta--warn">
            {t('loom.weave.loraWarning', { time: fmtClock(RENDER.mergePerLora * requestLoras.length) })}
          </p>
        ) : null}
        {/* Growing the form re-places the panel inside the viewport. */}
        <details className="loom-weave__advanced" onToggle={() => window.dispatchEvent(new Event('resize'))}>
          <summary>{t('loom.weave.advanced')}</summary>
          <Sa3AdvancedControls value={steering} onChange={setSteering} onSubmit={() => submit(null)} />
        </details>
        <div className="loom-weave__go">
          <Button variant="primary" disabled={!valid} onClick={() => submit(null)}>
            {t('loom.weave.go', { takes })}
          </Button>
          {decks.map(({ deck }) => (
            <Button key={deck} disabled={!valid} onClick={() => submit(deck)}>
              {t('loom.weave.goAndTie', { deck: t(deck === 'a' ? 'mixer.deckA' : 'mixer.deckB') })}
            </Button>
          ))}
        </div>
      </div>
    </AnchoredPanel>
  )
}
