import { useTranslation } from 'react-i18next'

import { HypercubeMark } from './HypercubeMark'

/** LSDJ brand lockup: the spinning-record/tumbling-hypercube mark beside the
 * LSDJ wordmark — LS neutral and DJ in the accent. The mark
 * inks from currentColor so it follows the master-accent token. The h1 carries
 * the accessible name; the visual text is hidden from the a11y tree so it isn't
 * read twice. */
export function Logo() {
  const { t } = useTranslation()
  return (
    <h1 className="logo" aria-label={t('app.title')}>
      <HypercubeMark className="logo__mark" />
      <span className="logo__text" aria-hidden="true">
        <span className="logo__word">
          <span className="logo__ls">{t('app.brand.ls')}</span>
          <span className="logo__dj">{t('app.brand.dj')}</span>
        </span>
        <span className="logo__tag">{t('app.tagline')}</span>
      </span>
    </h1>
  )
}
