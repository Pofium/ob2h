/**
 * Живые проверки против настоящего `ob2h serve` (не стаб).
 *
 * По умолчанию пропускаются, чтобы `node --test` оставался герметичным:
 *   OB2H_LIVE=1 node --test "tests/live.test.js"                     — только чтение
 *   OB2H_LIVE=1 OB2H_LIVE_WRITE=1 node --test "tests/live.test.js"   — плюс запись хода
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { Ob2hRpc } from '../lib/rpc.js'
import { Ob2hMemory } from '../lib/provider.js'

const BINARY =
  process.env.OB2H_BIN ?? 'C:/Projects/ob2h/target/release/ob2h.exe'
const DATA_DIR = process.env.OB2H_DATA_DIR ?? 'C:/Projects/ob2h/data'
const live = process.env.OB2H_LIVE === '1' && existsSync(BINARY)

const makeRpc = () =>
  new Ob2hRpc({
    command: BINARY,
    args: ['serve'],
    cwd: process.env.OB2H_CWD ?? 'C:/Projects/ob2h',
    env: { OB2H_DATA_DIR: DATA_DIR },
    startupTimeoutMs: 60_000,
    callTimeoutMs: 180_000,
  })

test('живой ob2h: handshake, 36 инструментов, блок памяти', { skip: !live }, async () => {
  const rpc = makeRpc()
  await rpc.start()
  try {
    const tools = await rpc.toolsList()
    assert.equal(tools.length, 36)
    assert.ok(tools.some((tool) => tool.name === 'memory_context'))

    const block = await rpc.toolCall('memory_context', {
      query: 'cordis-плагин DSH',
      max_tokens: 5,
      max_chars: 800,
    })
    assert.ok(block.startsWith('<agent_memory>'))
    assert.ok(block.length <= 1_200, `блок не должен превышать бюджет: ${block.length}`)
  } finally {
    await rpc.stop()
  }
})

test(
  'живой ob2h: автозахват хода доходит до daily-лога',
  { skip: !live || process.env.OB2H_LIVE_WRITE !== '1' },
  async () => {
    const rpc = makeRpc()
    const memory = new Ob2hMemory({
      rpc,
      source: 'dsh-live-test',
      author: 'dsh-live-test',
      writeTimeoutMs: 120_000,
    })
    try {
      const sessionId = `dsh-live-test-${Date.now()}`
      const marker = `маркер живого теста ${sessionId}`
      memory.recordTurn(sessionId, marker, 'ответ живого теста')
      await memory.flush()
      assert.equal(memory.stats.ingests, 1)

      const found = await rpc.toolCall('memory_search', { query: marker, limit: 3 })
      assert.match(found, /daily|dsh-live-test|маркер/i)
    } finally {
      await memory.close()
    }
  },
)
