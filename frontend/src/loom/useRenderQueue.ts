/** The render queue driven by the loom (plan §6): one queue for the hook's
 * lifetime, its jobs following the loom's rendering nodes and its results
 * folded back as loom actions. `progress` feeds the ghost bars; `pending` is
 * what render ahead must not weave behind twice. */

import { useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { invoke } from '../audio/nativeEngine'
import { postSa3Generate, postSa3GenerateWithSource } from '../generation/client'
import { saveGeneratedSong } from '../generation/library'
import type { RenderProgress } from './LoomView'
import { nodeById, type Loom, type LoomAction } from './loomState'
import { createRenderQueue, type RenderQueue } from './renderQueue'

export function useRenderQueue(loom: Loom, act: (action: LoomAction) => void) {
  const { t } = useTranslation()
  const [progress, setProgress] = useState<Record<string, RenderProgress>>({})

  // Made on first use, outside render: one queue for the hook's lifetime.
  const queueRef = useRef<RenderQueue | null>(null)
  const renderQueue = useCallback(() => {
    const clearProgress = (id: string) =>
      setProgress((current) => {
        const next = { ...current }
        delete next[id]
        return next
      })
    queueRef.current ??= createRenderQueue({
      sourceClip: (file, start, end) => invoke<ArrayBuffer>('song_source_clip', { name: file, start, end }),
      generate: postSa3Generate,
      generateWithSource: postSa3GenerateWithSource,
      save: saveGeneratedSong,
      onDone: (id, file, durationSeconds) => {
        clearProgress(id)
        act({ type: 'rendered', id, file, durationSeconds })
      },
      onError: (id, error) => {
        clearProgress(id)
        act({ type: 'render_failed', id, error })
      },
      onProgress: (id, elapsed, eta) => setProgress((current) => ({ ...current, [id]: { elapsed, eta } })),
    })
    return queueRef.current
  }, [act])

  // A rendering node gets one job once its parent is ready (the job reads the
  // parent's file); a node gone from the loom (a discarded take) drops its job.
  useEffect(() => {
    const { nodes } = loom
    const live = new Set(nodes.map((n) => n.id))
    const queue = renderQueue()
    const pending = new Set(queue.pending())
    for (const id of pending) if (!live.has(id)) queue.cancel(id)
    for (const node of nodes) {
      if (node.status !== 'rendering' || pending.has(node.id)) continue
      const parent = nodeById(nodes, node.parentId)
      if (parent && parent.status !== 'ready') {
        if (parent.status !== 'rendering') {
          act({ type: 'render_failed', id: node.id, error: t('loom.status.parentFailed') })
        }
        continue
      }
      const takes = node.takeGroup ? nodes.filter((n) => n.takeGroup === node.takeGroup) : []
      queue.enqueue({ nodeId: node.id, node, parent, take: takes.length > 1 ? takes.indexOf(node) + 1 : undefined })
    }
  }, [loom, renderQueue, act, t])

  const pending = useCallback(() => renderQueue().pending(), [renderQueue])
  return { progress, pending }
}
