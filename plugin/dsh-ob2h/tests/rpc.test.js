/** Юнит-тесты stdio JSON-RPC клиента (стаб ob2h serve, без сети и БД). */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Ob2hRpc, RpcDead, RpcError } from '../lib/rpc.js'

const here = dirname(fileURLToPath(import.meta.url))
const fakeServer = join(here, 'fake_server.js')

const makeRpc = (extra = {}) =>
  new Ob2hRpc({
    command: process.execPath,
    args: [fakeServer],
    callTimeoutMs: 5_000,
    startupTimeoutMs: 10_000,
    restartBackoffMs: 0,
    ...extra,
  })

test('handshake и tools/list', async () => {
  const rpc = makeRpc()
  await rpc.start()
  try {
    assert.equal(rpc.alive(), true)
    const tools = await rpc.toolsList()
    assert.deepEqual(
      tools.map((tool) => tool.name).sort(),
      ['memory_context', 'memory_search', 'session_ingest'],
    )
  } finally {
    await rpc.stop()
  }
})

test('tools/call возвращает склеенный текст контента', async () => {
  const rpc = makeRpc()
  await rpc.start()
  try {
    const text = await rpc.toolCall('memory_context', { query: 'кофе' })
    assert.match(text, /<agent_memory>/)
    assert.match(text, /кофе/)
  } finally {
    await rpc.stop()
  }
})

test('ошибка JSON-RPC приходит как RpcError', async () => {
  const rpc = makeRpc()
  await rpc.start()
  try {
    await assert.rejects(() => rpc.call('no/such/method', {}), RpcError)
  } finally {
    await rpc.stop()
  }
})

test('таймаут вызова отваливается по RpcError', async () => {
  const rpc = makeRpc()
  await rpc.start()
  try {
    await assert.rejects(() => rpc.call('hang', {}, 150), /таймаут/)
  } finally {
    await rpc.stop()
  }
})

test('start идемпотентен, stop закрывает процесс', async () => {
  const rpc = makeRpc()
  await rpc.start()
  const firstPid = rpc.proc.pid
  await rpc.start()
  assert.equal(rpc.proc.pid, firstPid)

  await rpc.stop()
  assert.equal(rpc.alive(), false)
  await assert.rejects(() => rpc.call('ping', {}), RpcDead)
})

test('проводной формат — newline-delimited JSON-RPC (журнал стаба)', async () => {
  const rpc = makeRpc()
  await rpc.start()
  try {
    await rpc.toolCall('memory_context', { query: 'провод' })
    const journal = JSON.parse(await rpc.toolCall('test_journal', {}))
    const init = journal.find((entry) => entry.method === 'initialize')
    assert.equal(init.params.protocolVersion, '2024-11-05')
    const call = journal.find(
      (entry) => entry.method === 'tools/call' && entry.params.name === 'memory_context',
    )
    assert.deepEqual(call.params.arguments, { query: 'провод' })
  } finally {
    await rpc.stop()
  }
})

test('ping недоступного процесса не бросает (fail-open)', async () => {
  const rpc = makeRpc({ command: join(here, 'no-such-binary.exe') })
  assert.equal(await rpc.ping(200), false)
})
