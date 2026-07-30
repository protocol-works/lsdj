import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from 'react'
import { createPortal } from 'react-dom'

const VIEWPORT_GAP = 8

type Position = { top: number; left: number }

/** A non-modal panel anchored to a trigger but portalled above the app's
 * scrolling/clipping deck and media surfaces. Escape/outside click dismiss it;
 * focus enters on open and returns to the trigger on close. */
export function AnchoredPanel({
  open,
  anchorRef,
  onClose,
  label,
  className = '',
  children,
}: {
  open: boolean
  anchorRef: RefObject<HTMLElement | null>
  onClose: () => void
  label: string
  className?: string
  children: ReactNode
}) {
  const panelRef = useRef<HTMLDivElement>(null)
  const onCloseRef = useRef(onClose)
  const [position, setPosition] = useState<Position | null>(null)

  useEffect(() => {
    onCloseRef.current = onClose
  }, [onClose])

  useLayoutEffect(() => {
    if (!open) return

    const anchorAtOpen = anchorRef.current

    const updatePosition = () => {
      const anchor = anchorRef.current
      const panel = panelRef.current
      if (!anchor || !panel) return
      const anchorBox = anchor.getBoundingClientRect()
      const panelBox = panel.getBoundingClientRect()
      const roomBelow = window.innerHeight - anchorBox.bottom - VIEWPORT_GAP
      const roomAbove = anchorBox.top - VIEWPORT_GAP
      const top =
        roomBelow >= panelBox.height || roomBelow >= roomAbove
          ? anchorBox.bottom + VIEWPORT_GAP
          : Math.max(VIEWPORT_GAP, anchorBox.top - panelBox.height - VIEWPORT_GAP)
      const left = Math.min(
        Math.max(VIEWPORT_GAP, anchorBox.left),
        Math.max(VIEWPORT_GAP, window.innerWidth - panelBox.width - VIEWPORT_GAP),
      )
      setPosition((current) =>
        current?.top === top && current.left === left ? current : { top, left },
      )
    }

    updatePosition()
    panelRef.current?.focus()

    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node
      if (panelRef.current?.contains(target) || anchorRef.current?.contains(target)) return
      onCloseRef.current()
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      event.stopPropagation()
      onCloseRef.current()
    }

    document.addEventListener('pointerdown', onPointerDown, true)
    document.addEventListener('keydown', onKeyDown, true)
    window.addEventListener('resize', updatePosition)
    window.addEventListener('scroll', updatePosition, true)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown, true)
      document.removeEventListener('keydown', onKeyDown, true)
      window.removeEventListener('resize', updatePosition)
      window.removeEventListener('scroll', updatePosition, true)
      anchorAtOpen?.focus()
    }
  }, [anchorRef, open])

  if (!open || typeof document === 'undefined') return null

  return createPortal(
    <div
      ref={panelRef}
      className={`ui-anchored-panel${className ? ` ${className}` : ''}`}
      role="dialog"
      aria-label={label}
      tabIndex={-1}
      style={{
        top: position?.top ?? 0,
        left: position?.left ?? 0,
        visibility: position ? 'visible' : 'hidden',
      }}
    >
      {children}
    </div>,
    document.body,
  )
}
