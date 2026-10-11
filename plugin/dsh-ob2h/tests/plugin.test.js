/**
 * Тесты самого плагина: подставной cordis-ctx + стаб ob2h serve.
 * Проверяют контракт с харнессом без его запуска.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import {
  apply,
  blocksToText,
  createLogger,
  install,
  queryFromMessages,
  resolveOptions,
  snapshotMessage,
} from '../index.js'
import { Ob2hMemory } from '../lib/provider.js'
import { Ob2hRpc } from '../lib/rpc.js'

const here = dirname(fileURLToPath(import.meta.url))
const fakeServer = join(here, 'fake_server.js')

/** Минимальный двойник cordis-контекста. */
function fakeContext({ agents } = {}) {
  const listeners = new Map()
  const sections = []
  const disposers = []
  const ctx = {
    logger: { info() {}, warn() {}, debug() {} },
    systemPrompt: { section: (section) => sections.push(section) },
    on: (event, handler, options) => {
      const list = listeners.get(event) ?? []
      list.push({ handler, options })
      listeners.set(event, list)
    },
    effect: (callback) => disposers.push(callback()),
    get: () => agents,
  }
  return {
    ctx,
    sections,
    disposers,
    emit: (event, ...args) =>
      Promise.all((listeners.get(event) ?? []).map(({ handler }) => handler(...args))),
    preStep: (payload, next) => {
      const entry = (listeners.get('agent/pre-step') ?? [])[0]
      assert.ok(entry, 'плагин должен подписаться на agent/pre-step')
      return entry.handler(payload, next)
    },
  }
}

const makeResources = (options) => {
  const logger = { info() {}, warn() {}, debug() {} }
  const rpc = new Ob2hRpc({
    command: process.execPath,
    args: [fakeServer],
    callTimeoutMs: 5_000,
    startupTimeoutMs: 10_000,
    restartBackoffMs: 0,
    logger,
  })
  const memory = new Ob2hMemory({
    rpc,
    source: options.source,
    author: options.author,
    prefetchTimeoutMs: 5_000,
    maxTokens: options.maxTokens,
    maxChars: options.maxChars,
    logger,
  })
  return { rpc, memory }
}

const options = (extra = {}) =>
  resolveOptions({ binary: process.execPath, recallTimeoutMs: 5_000, ...extra })

const entry = (text, kind = 'user') => ({
  role: 'user',
  content: [{ type: 'text', text }],
  source: { kind },
})

test('заметка регистрируется как секция системного промпта', () => {
  const harness = fakeContext()
  const opts = options()
  const resources = makeResources(opts)
  try {
    install(harness.ctx, opts, resources)
    assert.equal(harness.sections.length, 1)
    assert.equal(harness.sections[0].name, 'ob2h:note')
    assert.ok(harness.sections[0].order > 10200, 'секция должна идти после persona-suffix')
    assert.match(harness.sections[0].text, /OB2H/)
  } finally {
    void resources.memory.close()
  }
})

test('apply без binary не падает и ничего не регистрирует', () => {
  const harness = fakeContext()
  assert.equal(apply(harness.ctx, {}), undefined)
  assert.equal(harness.sections.length, 0)
})

test('recall подмешивает <agent_memory> в батч хода', async () => {
  const harness = fakeContext()
  const opts = options()
  const resources = makeResources(opts)
  try {
    install(harness.ctx, opts, resources)
    const decision = await harness.preStep(
      { step: 1, signal: { aborted: false }, agent: { id: 'a1', session: { id: 's1' } } },
      async () => ({ kind: 'enter', messages: [entry('что мы знаем про кофе')] }),
    )
    assert.equal(decision.messages.length, 2)
    const injected = decision.messages[1]
    assert.equal(injected.role, 'user')
    assert.equal(injected.source.kind, 'ob2h')
    assert.match(injected.content[0].text, /<agent_memory>/)
    assert.match(injected.content[0].text, /кофе/)
  } finally {
    await resources.memory.close()
  }
})

test('recall не срабатывает на шагах > 1 и на инъекциях плагинов', async () => {
  const harness = fakeContext()
  const opts = options()
  const resources = makeResources(opts)
  try {
    install(harness.ctx, opts, resources)
    const later = await harness.preStep(
      { step: 2, signal: { aborted: false }, agent: { id: 'a1', session: { id: 's1' } } },
      async () => ({ kind: 'enter', messages: [entry('что мы знаем про кофе')] }),
    )
    assert.equal(later.messages.length, 1)

    const injectedOnly = await harness.preStep(
      { step: 1, signal: { aborted: false }, agent: { id: 'a1', session: { id: 's1' } } },
      async () => ({ kind: 'enter', messages: [entry('служебный текст', 'time-context')] }),
    )
    assert.equal(injectedOnly.messages.length, 1)
  } finally {
    await resources.memory.close()
  }
})

test('recall не ломает ход при reject и при мёртвом бинарнике', async () => {
  const harness = fakeContext()
  const opts = options()
  const resources = makeResources(opts)
  try {
    install(harness.ctx, opts, resources)
    const rejected = await harness.preStep(
      { step: 1, signal: { aborted: false } },
      async () => ({ kind: 'reject' }),
    )
    assert.deepEqual(rejected, { kind: 'reject' })
  } finally {
    await resources.memory.close()
  }

  const broken = fakeContext()
  const brokenOpts = options()
  const brokenResources = {
    rpc: new Ob2hRpc({
      command: join(here, 'no-such-binary.exe'),
      restartBackoffMs: 0,
      callTimeoutMs: 300,
      startupTimeoutMs: 300,
    }),
    memory: null,
  }
  brokenResources.memory = new Ob2hMemory({
    rpc: brokenResources.rpc,
    prefetchTimeoutMs: 300,
    writeTimeoutMs: 300,
  })
  install(broken.ctx, brokenOpts, brokenResources)
  const decision = await broken.preStep(
    { step: 1, signal: { aborted: false }, agent: { id: 'a1', session: { id: 's1' } } },
    async () => ({ kind: 'enter', messages: [entry('привет')] }),
  )
  assert.equal(decision.messages.length, 1)
  await brokenResources.memory.close()
})

test('захват хода: turn/start → user/assistant → turn/end пишет session_ingest', async () => {
  const harness = fakeContext()
  const opts = options()
  const resources = makeResources(opts)
  try {
    install(harness.ctx, opts, resources)
    const session = { id: 's-capture' }
    await harness.emit('session/event', session, { type: 'turn/start', data: { turn: 1 } })
    await harness.emit('session/event', session, {
      type: 'user/message',
      data: { ...entry('вопрос человека'), source: { kind: 'user', rpcId: 'r1' } },
    })
    // Инъекция плагина не должна попасть в память.
    await harness.emit('session/event', session, {
      type: 'user/message',
      data: entry('снапшот времени', 'time-context'),
    })
    await harness.emit('session/event', session, {
      type: 'assistant/message',
      data: { message: { role: 'assistant', content: [{ type: 'text', text: 'ответ модели' }] } },
    })
    await harness.emit('session/event', session, { type: 'turn/end', data: { turn: 1 } })
    await resources.memory.flush()

    const journal = JSON.parse(await resources.rpc.toolCall('test_journal', {}))
    const ingests = journal.filter(
      (item) => item.method === 'tools/call' && item.params.name === 'session_ingest',
    )
    assert.equal(ingests.length, 1)
    assert.deepEqual(ingests[0].params.arguments.messages, [
      { role: 'user', content: 'вопрос человека' },
      { role: 'assistant', content: 'ответ модели' },
    ])
    assert.equal(ingests[0].params.arguments.source, 'dsh')
    assert.equal(ingests[0].params.arguments.session_id, 's-capture')
  } finally {
    await resources.memory.close()
  }
})

test('сабагентские сессии не пишутся в память', async () => {
  const agents = {
    list: () => [{ id: 'child' }, { id: 'root' }],
    isOwnedBy: (id, parent) => id === 'child' && parent === 'root',
    get: (id) => ({ id }),
  }
  const harness = fakeContext({ agents })
  const opts = options()
  const resources = makeResources(opts)
  try {
    install(harness.ctx, opts, resources)
    const child = { id: 'child' }
    await harness.emit('session/event', child, { type: 'turn/start', data: { turn: 1 } })
    await harness.emit('session/event', child, {
      type: 'user/message',
      data: { ...entry('ход сабагента'), source: { kind: 'user' } },
    })
    await harness.emit('session/event', child, {
      type: 'assistant/message',
      data: { message: { content: [{ type: 'text', text: 'ответ сабагента' }] } },
    })
    await harness.emit('session/event', child, { type: 'turn/end', data: { turn: 1 } })
    await resources.memory.flush()

    const journal = JSON.parse(await resources.rpc.toolCall('test_journal', {}))
    const ingests = journal.filter(
      (item) => item.method === 'tools/call' && item.params.name === 'session_ingest',
    )
    assert.equal(ingests.length, 0)
  } finally {
    await resources.memory.close()
  }
})

test('файловый лог фиксирует решения recall и захвата', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ob2h-plugin-'))
  const logFile = join(dir, 'logs', 'dsh-plugin.log')
  const harness = fakeContext()
  const opts = options({ logFile, logEvents: true })
  const resources = makeResources(opts)

  const waitFor = async (pattern) => {
    const deadline = Date.now() + 8_000
    while (Date.now() < deadline) {
      try {
        const text = readFileSync(logFile, 'utf8')
        if (pattern.test(text)) return text
      } catch {
        // файла ещё нет
      }
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
    return readFileSync(logFile, 'utf8')
  }

  try {
    const logger = createLogger(harness.ctx, opts)
    assert.equal(logger.file, logFile)
    install(harness.ctx, opts, resources, logger)

    // 1. recall: подмешивание блока.
    await harness.preStep(
      { step: 1, signal: { aborted: false }, agent: { id: 'a1', session: { id: 's-log' } } },
      async () => ({ kind: 'enter', messages: [entry('что мы знаем про кофе')] }),
    )
    // 2. turn/end без предшествующего turn/start — плагин загрузился в середине хода.
    await harness.emit('session/event', { id: 's-log' }, { type: 'turn/end', data: { turn: 7 } })
    // 3. Полный ход.
    await harness.emit('session/event', { id: 's-log' }, { type: 'turn/start', data: { turn: 8 } })
    await harness.emit('session/event', { id: 's-log' }, {
      type: 'user/message',
      data: { ...entry('вопрос человека'), source: { kind: 'user' } },
    })
    await harness.emit('session/event', { id: 's-log' }, {
      type: 'user/message',
      data: { ...entry('уточнение по ходу'), source: { kind: 'user' } },
    })
    await harness.emit('session/event', { id: 's-log' }, {
      type: 'assistant/message',
      data: { message: { content: [{ type: 'text', text: 'ответ модели' }] } },
    })
    await harness.emit('session/event', { id: 's-log' }, { type: 'turn/end', data: { turn: 8 } })
    await resources.memory.flush()

    // Обе реплики человека попали в одну запись хода (руление посреди хода не теряется).
    const journal = JSON.parse(await resources.rpc.toolCall('test_journal', {}))
    const ingest = journal.find(
      (item) => item.method === 'tools/call' && item.params?.name === 'session_ingest',
    )
    assert.ok(ingest, 'плагин должен вызвать session_ingest')
    assert.equal(ingest.params.arguments.session_id, 's-log')
    assert.equal(ingest.params.arguments.source, options().source)
    const userMessage = ingest.params.arguments.messages.find((m) => m.role === 'user')
    assert.equal(userMessage.content, 'вопрос человека\n\nуточнение по ходу')
    const assistantMessage = ingest.params.arguments.messages.find((m) => m.role === 'assistant')
    assert.equal(assistantMessage.content, 'ответ модели')

    const log = await waitFor(/записан ход/)
    assert.match(log, /INFO память подключена/)
    assert.match(log, /INFO pre-step: подмешан блок памяти/)
    assert.match(log, /INFO turn\/end s-log: пропущен — нет состояния/)
    assert.match(log, /INFO turn\/end s-log: записан ход/)
    // verbose-режим трассирует каждое событие и содержимое хода
    assert.match(log, /EVENT event turn\/start s-log/)
    assert.match(log, /EVENT user\/message s-log: \d+ симв\./)
  } finally {
    await resources.memory.close()
  }
})

test('вспомогательные функции', () => {
  assert.equal(blocksToText([{ type: 'text', text: 'а' }, { type: 'image' }]), 'а')
  assert.equal(blocksToText('строка'), 'строка')
  assert.equal(blocksToText(undefined), '')

  assert.equal(queryFromMessages([entry('первый'), entry('второй')]), 'второй')
  assert.equal(queryFromMessages([entry('служебное', 'time-context')]), '')
  assert.equal(queryFromMessages([]), '')

  const snapshot = snapshotMessage('<agent_memory>\n- факт\n</agent_memory>')
  assert.equal(snapshot.role, 'user')
  assert.equal(snapshot.source.kind, 'ob2h')
  assert.equal(snapshot.source.form, 'snapshot')
  assert.ok(Object.isFrozen(snapshot))
  assert.ok(Object.isFrozen(snapshot.content[0]))
  assert.equal(snapshot.source.sections[0].name, 'ob2h')
})
