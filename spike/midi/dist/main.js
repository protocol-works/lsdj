// LSDJ MIDI Spike C harness.
//
// All MIDI here is the plain WebMIDI API. In a Tauri build, tauri-plugin-midi
// injects a polyfill at webview startup that provides navigator.requestMIDIAccess
// over midir/CoreMIDI (SysEx + output send included). No import is needed.

// --- DDJ-FLX4 constants (from docs/midi-ddj-flx4.md) ----------------------
// Position-query SysEx: makes the controller flood back every analog
// control's current position as a burst of CC/Note input.
const POSITION_QUERY = [
  0xf0, 0x00, 0x40, 0x05, 0x00, 0x00, 0x04, 0x05, 0x00, 0x50, 0x02, 0xf7,
]
// HOT CUE pads 1-8, deck 1: Note On, channel 7 (0x97), notes 0x00-0x07.
// velocity 0x7F lights the LED, 0x00 clears it.
const PAD_STATUS = 0x97
const PAD_NOTES = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]
const FLX4_RE = /FLX4/i
const FLOOD_WINDOW_MS = 500

// --- DOM refs --------------------------------------------------------------
const $ = (id) => document.getElementById(id)
const els = {
  nodevice: $('nodevice'),
  stDev: $('st-dev'),
  stAccess: $('st-access'),
  stIncount: $('st-incount'),
  stSysex: $('st-sysex'),
  stFlood: $('st-flood'),
  stOut: $('st-out'),
  selIn: $('sel-in'),
  selOut: $('sel-out'),
  pillPorts: $('pill-ports'),
  btnQuery: $('btn-query'),
  btnLight: $('btn-light'),
  btnClear: $('btn-clear'),
  btnClearLog: $('btn-clearlog'),
  log: $('log'),
}

// --- state -----------------------------------------------------------------
let access = null
let currentInput = null // MIDIInput we attached a listener to
let currentInputId = ''
let currentOutputId = ''
let inCount = 0
let sysexCount = 0
let floodCounting = false
let floodCount = 0
let didAutoQuery = false

// --- helpers ---------------------------------------------------------------
function hex(bytes) {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0').toUpperCase())
    .join(' ')
}

function setV(el, text, cls) {
  el.textContent = text
  el.className = 'v' + (cls ? ' ' + cls : '')
}

function logLine(text, cls) {
  const div = document.createElement('div')
  div.className = 'logline' + (cls ? ' ' + cls : '')
  div.textContent = text
  els.log.appendChild(div)
  els.log.scrollTop = els.log.scrollHeight
  // Cap the log so a flood doesn't grow it unbounded.
  while (els.log.childElementCount > 2000) els.log.removeChild(els.log.firstChild)
}

// --- input handling --------------------------------------------------------
function onMidiMessage(event) {
  const data = event.data
  inCount += 1
  els.stIncount.textContent = String(inCount)

  const isSysex = data.length > 0 && data[0] === 0xf0
  if (isSysex) {
    sysexCount += 1
    els.stSysex.textContent = String(sysexCount)
  }

  if (floodCounting) floodCount += 1

  logLine(`< ${hex(data)}${isSysex ? '   [SysEx]' : ''}`, isSysex ? 'sysex' : '')
}

function attachInput(id) {
  // Detach any prior input.
  if (currentInput) {
    currentInput.onmidimessage = null
    currentInput = null
  }
  currentInputId = id || ''
  if (!id || !access) return
  const input = access.inputs.get(id)
  if (!input) return
  currentInput = input
  // Setting onmidimessage causes the shim to open the port and start streaming.
  input.onmidimessage = onMidiMessage
  logLine(`# opened input: ${input.name}`, 'sys')
}

// --- output helpers --------------------------------------------------------
function getOutput() {
  if (!access || !currentOutputId) return null
  return access.outputs.get(currentOutputId) || null
}

function sendPositionQuery() {
  const out = getOutput()
  if (!out) return
  // Start a 500 ms flood-count window starting from this query.
  floodCount = 0
  floodCounting = true
  setV(els.stFlood, 'counting…', 'warn')
  out.send(POSITION_QUERY)
  logLine(`> ${hex(POSITION_QUERY)}   [position query]`, 'out')
  setV(els.stOut, 'position query sent', 'ok')

  setTimeout(() => {
    floodCounting = false
    const got = floodCount > 0
    setV(
      els.stFlood,
      `${got ? 'YES' : 'NO'} — ${floodCount} msgs in ${FLOOD_WINDOW_MS} ms`,
      got ? 'ok' : 'bad'
    )
    logLine(
      `# position flood: ${floodCount} messages in ${FLOOD_WINDOW_MS} ms after query`,
      'sys'
    )
  }, FLOOD_WINDOW_MS)
}

function setPads(on) {
  const out = getOutput()
  if (!out) return
  const vel = on ? 0x7f : 0x00
  for (const note of PAD_NOTES) {
    const msg = [PAD_STATUS, note, vel]
    out.send(msg)
    logLine(`> ${hex(msg)}   [pad ${note} ${on ? 'on' : 'off'}]`, 'out')
  }
  setV(els.stOut, on ? 'lit pads 1-8 (deck 1)' : 'cleared pads 1-8', 'ok')
}

// --- device list / selection ----------------------------------------------
function fillSelect(sel, ports, selectedId) {
  const prev = selectedId
  sel.innerHTML = '<option value="">— none —</option>'
  for (const [id, port] of ports) {
    const opt = document.createElement('option')
    opt.value = id
    opt.textContent = port.name
    if (id === prev) opt.selected = true
    sel.appendChild(opt)
  }
}

function pickFlx4(ports) {
  for (const [id, port] of ports) {
    if (FLX4_RE.test(port.name)) return id
  }
  return ''
}

function refreshDevices() {
  if (!access) return
  const inputs = access.inputs
  const outputs = access.outputs

  els.pillPorts.textContent = `${inputs.size} in / ${outputs.size} out`

  // Auto-select an FLX4 if nothing chosen yet (or current selection vanished).
  if (!currentInputId || !inputs.has(currentInputId)) {
    const pick = pickFlx4(inputs)
    if (pick) attachInput(pick)
    else currentInputId = ''
  }
  if (!currentOutputId || !outputs.has(currentOutputId)) {
    currentOutputId = pickFlx4(outputs)
  }

  fillSelect(els.selIn, inputs, currentInputId)
  fillSelect(els.selOut, outputs, currentOutputId)

  const flx4Present = pickFlx4(inputs) !== '' || pickFlx4(outputs) !== ''
  els.nodevice.classList.toggle('hidden', flx4Present)

  const haveOut = !!getOutput()
  els.btnQuery.disabled = !haveOut
  els.btnLight.disabled = !haveOut
  els.btnClear.disabled = !haveOut

  const devName =
    (currentInput && currentInput.name) ||
    (getOutput() && getOutput().name) ||
    (inputs.size + outputs.size > 0 ? 'connected (no FLX4 match)' : '—')
  setV(els.stDev, devName, flx4Present ? 'ok' : inputs.size + outputs.size ? 'warn' : 'bad')

  // Auto-send the position query once, the first time we have an FLX4 output.
  if (!didAutoQuery && haveOut && FLX4_RE.test(getOutput().name)) {
    didAutoQuery = true
    logLine('# auto-sending position query on connect…', 'sys')
    sendPositionQuery()
  }
}

// --- wiring ----------------------------------------------------------------
els.selIn.addEventListener('change', (e) => attachInput(e.target.value))
els.selOut.addEventListener('change', (e) => {
  currentOutputId = e.target.value
  refreshDevices()
})
els.btnQuery.addEventListener('click', sendPositionQuery)
els.btnLight.addEventListener('click', () => setPads(true))
els.btnClear.addEventListener('click', () => setPads(false))
els.btnClearLog.addEventListener('click', () => {
  els.log.innerHTML = ''
})

async function init() {
  if (!navigator.requestMIDIAccess) {
    setV(els.stAccess, 'unavailable (no shim)', 'bad')
    logLine('# navigator.requestMIDIAccess is not defined — plugin not loaded?', 'sys')
    return
  }
  try {
    access = await navigator.requestMIDIAccess({ sysex: true })
    setV(els.stAccess, 'resolved (sysex)', 'ok')
    logLine('# requestMIDIAccess resolved', 'sys')

    // Re-render whenever ports connect/disconnect (1 s poll in the plugin).
    access.onstatechange = (e) => {
      if (e && e.port) {
        logLine(`# statechange: ${e.port.type} ${e.port.name} -> ${e.port.state}`, 'sys')
      }
      refreshDevices()
    }
    refreshDevices()
  } catch (err) {
    setV(els.stAccess, 'rejected', 'bad')
    logLine('# requestMIDIAccess failed: ' + (err && err.message ? err.message : err), 'sys')
  }
}

init()
