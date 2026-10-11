/** Юнит-тесты логики провайдера: автозахват ходов, prefetch, fail-open. */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Ob2hMemory, countRecords } from '../lib/provider.js'
import { Ob2hRpc } from '../lib/rpc.js'

const here = dirname(fileURLToPath(import.meta.url))
const fakeServer = join(here, 'fake_server.js')

const makeProvider = (extra = {}) => {
  const rpc =
    extra.rpc ??
    new Ob2hRpc({
      command: process.execPath,
      args: [fakeServer],
      callTimeoutMs: 5_000,
      startupTimeoutMs: 10_000,
      restartBackoffMs: 0,
    })
  const memory = new Ob2hMemory({
    rpc,
    source: 'dsh',
    author: 'dsh-test',
    prefetchTimeoutMs: 5_000,
    writeTimeoutMs: 10_000,
    ...extra.memory,
  })
  return { rpc, memory }
}

const journalOf = async (rpc) => JSON.parse(await rpc.toolCall('test_journal', {}))

const waitFor = async (predicate, timeoutMs = 3_000) => {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (predicate()) return true
    await new Promise((resolve) => setTimeout(resolve, 10))
  }
  return false
}

test('каждый ход уходит полным префиксом сессии (дедуп на сервере)', async () => {
  const { rpc, memory } = makeProvider()
  try {
    memory.recordTurn('s-1', 'вопрос 1', 'ответ 1')
    await memory.flush()
    memory.recordTurn('s-1', 'вопрос 2', 'ответ 2')
    await memory.flush()

    const ingests = (await journalOf(rpc)).filter(
      (entry) => entry.method === 'tools/call' && entry.params.name === 'session_ingest',
    )
    assert.equal(ingests.length, 2)
    assert.equal(ingests[0].params.arguments.messages.length, 2)
    assert.equal(ingests[1].params.arguments.messages.length, 4)
    assert.equal(ingests[1].params.arguments.source, 'dsh')
    assert.equal(ingests[1].params.arguments.session_id, 's-1')
    assert.equal(ingests[1].params.arguments.author, 'dsh-test')
    assert.deepEqual(ingests[1].params.arguments.messages[2], {
      role: 'user',
      content: 'вопрос 2',
    })
  } finally {
    await memory.close()
  }
})

test('сессии не смешиваются: у каждой свой аккумулятор', async () => {
  const { rpc, memory } = makeProvider()
  try {
    memory.recordTurn('a', 'в a', 'из a')
    memory.recordTurn('b', 'в b', 'из b')
    await memory.flush()

    const ingests = (await journalOf(rpc)).filter(
      (entry) => entry.method === 'tools/call' && entry.params.name === 'session_ingest',
    )
    const bySession = new Map(
      ingests.map((entry) => [
        entry.params.arguments.session_id,
        entry.params.arguments.messages.length,
      ]),
    )
    assert.equal(bySession.get('a'), 2)
    assert.equal(bySession.get('b'), 2)
  } finally {
    await memory.close()
  }
})

test('prefetch: блок <agent_memory> забирается синхронно и ровно один раз', async () => {
  const { memory } = makeProvider()
  try {
    memory.queuePrefetch('s-2', 'кофе')
    const ready = await waitFor(() => memory.prefetchCache.has('s-2'))
    assert.equal(ready, true)

    const block = memory.takePrefetch('s-2')
    assert.match(block, /^<agent_memory>/)
    assert.match(block, /кофе/)
    assert.equal(memory.takePrefetch('s-2'), '')
  } finally {
    await memory.close()
  }
})

test('пустой memory_context добирается fallback-поиском', async () => {
  const { memory } = makeProvider()
  try {
    memory.queuePrefetch('s-3', 'empty')
    await waitFor(() => memory.prefetchCache.has('s-3'))
    const block = memory.takePrefetch('s-3')
    assert.match(block, /^<agent_memory>/)
    assert.match(block, /fact-1/)
  } finally {
    await memory.close()
  }
})

test('нет ни контекста, ни хитов — пустая строка, без мусора', async () => {
  const { memory } = makeProvider()
  try {
    memory.queuePrefetch('s-5', 'nothing')
    await waitFor(() => memory.prefetchCache.has('s-5'))
    assert.equal(memory.takePrefetch('s-5'), '')
  } finally {
    await memory.close()
  }
})

test('fail-open: мёртвый бинарник не ломает ход', async () => {
  const { memory } = makeProvider({
    rpc: new Ob2hRpc({
      command: join(here, 'no-such-binary.exe'),
      restartBackoffMs: 0,
      startupTimeoutMs: 500,
      callTimeoutMs: 500,
    }),
  })
  memory.recordTurn('s-4', 'привет', 'пока')
  memory.queuePrefetch('s-4', 'что-нибудь')
  await memory.flush()
  assert.equal(memory.takePrefetch('s-4'), '')
  assert.ok(memory.stats.errors >= 1)
  await memory.close()
})

test('countRecords считает содержательные строки', () => {
  assert.equal(countRecords(''), 0)
  assert.equal(countRecords('<agent_memory>\n- один\n- два\n</agent_memory>'), 2)
})
