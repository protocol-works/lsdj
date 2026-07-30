import { useTranslation } from 'react-i18next'

import { Button } from '../ui/Button'
import { Select } from '../ui/Select'
import { Slider } from '../ui/Slider'
import { Switch } from '../ui/Switch'
import { TextField } from '../ui/TextField'
import {
  APG_DEFAULT,
  APG_MAX,
  APG_MIN,
  CFG_DEFAULT,
  CFG_MAX,
  CFG_MIN,
  hasNegativeConcept,
  parseFixedSeed,
  toggleNegativeConcept,
  type Sa3SteeringDraft,
} from './songGeneration'

const NEGATIVE_CONCEPTS = ['vocals', 'drums', 'cymbals', 'melody'] as const

/** Complete SA3 text-steering controls. API calls and persistence stay with the owner. */
export function Sa3AdvancedControls({
  value,
  onChange,
  onSubmit,
}: {
  value: Sa3SteeringDraft
  onChange: (value: Sa3SteeringDraft) => void
  onSubmit: () => void
}) {
  const { t } = useTranslation()
  const guidanceOff = value.guidance === 'off'
  const fixedSeedInvalid =
    value.seed.mode === 'fixed' && parseFixedSeed(value.seed.value) === null
  const update = (partial: Partial<Sa3SteeringDraft>) =>
    onChange({ ...value, ...partial })

  return (
    <section
      className="media__advanced"
      aria-label={t('media.generate.advanced.label')}
    >
      <fieldset className="media__guidance-section">
        <legend className="media__guidance-legend">
          <Switch
            label={t('media.generate.advanced.guidance', {
              state: t(`media.generate.advanced.states.${value.guidance}`),
            })}
            on={value.guidance === 'on'}
            onClick={() =>
              update({ guidance: value.guidance === 'on' ? 'off' : 'on' })
            }
          />
        </legend>
        <div
          className={`media__guidance-content${
            guidanceOff ? ' media__guidance-content--paused' : ''
          }`}
        >
          <p className="media__generation-hint">
            {t('media.generate.advanced.guidanceCost')}
          </p>
          <div className="media__guidance-body">
            <div className="media__advanced-negative">
              <TextField
                label={t('media.generate.advanced.negativePrompt')}
                placeholder={t('media.generate.advanced.negativePlaceholder')}
                value={value.negativePrompt}
                disabled={guidanceOff}
                onChange={(event) => update({ negativePrompt: event.target.value })}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') onSubmit()
                }}
              />
              <p className="media__generation-hint">
                {t('media.generate.advanced.negativeHint')}
              </p>
              <div className="media__negative-chips">
                {NEGATIVE_CONCEPTS.map((concept) => {
                  const selected = hasNegativeConcept(value.negativePrompt, concept)
                  return (
                    <Button
                      key={concept}
                      type="button"
                      lit={selected}
                      aria-pressed={selected}
                      disabled={guidanceOff}
                      onClick={() =>
                        update({
                          negativePrompt: toggleNegativeConcept(
                            value.negativePrompt,
                            concept,
                          ),
                        })
                      }
                    >
                      {t(`media.generate.advanced.concepts.${concept}`)}
                    </Button>
                  )
                })}
              </div>
            </div>
            <div className="media__advanced-controls">
              <Slider
                label={t('media.generate.advanced.cfg', { value: value.cfg.toFixed(1) })}
                help={{
                  label: t('media.generate.advanced.cfgHelpLabel'),
                  text: t('media.generate.advanced.cfgHelp'),
                }}
                min={CFG_MIN}
                max={CFG_MAX}
                step={0.1}
                value={value.cfg}
                disabled={guidanceOff}
                onChange={(cfg) => update({ cfg })}
                onReset={() => update({ cfg: CFG_DEFAULT })}
                resetLabel={t('media.generate.advanced.resetCfg')}
              />
              <Slider
                label={t('media.generate.advanced.apg', { value: value.apg.toFixed(1) })}
                help={{
                  label: t('media.generate.advanced.apgHelpLabel'),
                  text: t('media.generate.advanced.apgHelp'),
                }}
                min={APG_MIN}
                max={APG_MAX}
                step={0.1}
                value={value.apg}
                disabled={guidanceOff}
                onChange={(apg) => update({ apg })}
                onReset={() => update({ apg: APG_DEFAULT })}
                resetLabel={t('media.generate.advanced.resetApg')}
              />
            </div>
          </div>
        </div>
      </fieldset>
      <div className="media__seed-section">
        <div className="media__seed-fields">
          <Select
            label={t('media.generate.advanced.seed')}
            value={value.seed.mode}
            options={[
              { value: 'random', label: t('media.generate.advanced.randomSeed') },
              { value: 'fixed', label: t('media.generate.advanced.fixedSeed') },
            ]}
            onChange={(mode) =>
              update({
                seed:
                  mode === 'fixed'
                    ? { mode: 'fixed', value: '' }
                    : { mode: 'random' },
              })
            }
          />
          {value.seed.mode === 'fixed' && (
            <TextField
              label={t('media.generate.advanced.seedValue')}
              value={value.seed.value}
              inputMode="numeric"
              aria-invalid={fixedSeedInvalid}
              onChange={(event) =>
                update({ seed: { mode: 'fixed', value: event.target.value } })
              }
            />
          )}
        </div>
        {fixedSeedInvalid && (
          <p className="media__error" role="alert">
            {t('media.generate.advanced.seedInvalid')}
          </p>
        )}
      </div>
    </section>
  )
}
