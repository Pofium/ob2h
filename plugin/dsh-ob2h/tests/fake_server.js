/**
 * Стаб `ob2h serve` для юнит-тестов: построчный JSON-RPC, без сети, без БД.
 * Поддерживает журнал вызовов (инструмент `test_journal`) и «зависание» (метод `hang`).
 */

import { createInterface } from 'node:readline'

const journal = []

const reply = (id, result) =>
  process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id, result })}\n`)
const fail = (id, message) =>
  process.stdout.write(
    `${JSON.stringify({ jsonrpc: '2.0', id, error: { code: -32601, message } })}\n`,
  )

const rl = createInterface({ input: process.stdin })
rl.on('line', (line) => {
  let message
  try {
    message = JSON.parse(line)
  } catch {
    return
  }
  if (message.id === undefined || message.id === null) return
  const id = message.id
  const method = message.method
  journal.push({ method, params: message.params ?? null })

  switch (method) {
    case 'hang':
      return // намеренно без ответа — тест таймаута
    case 'initialize':
      return reply(id, {
        protocolVersion: '2024-11-05',
        capabilities: { tools: { listChanged: false } },
        serverInfo: { name: 'ob2h-stub', version: '0.0.0' },
      })
    case 'ping':
      return reply(id, {})
    case 'tools/list':
      return reply(id, {
        tools: [
          {
            name: 'memory_context',
            description: 'блок <agent_memory>',
            input_schema: { type: 'object', properties: { query: { type: 'string' } } },
          },
          {
            name: 'memory_search',
            description: 'поиск',
            input_schema: { type: 'object', properties: { query: { type: 'string' } } },
          },
          {
            name: 'session_ingest',
            description: 'запись ходов',
            input_schema: { type: 'object' },
          },
        ],
      })
    case 'tools/call': {
      const name = message.params?.name
      const args = message.params?.arguments ?? {}
      if (name === 'test_journal') {
        return reply(id, { content: [{ type: 'text', text: JSON.stringify(journal) }] })
      }
      if (name === 'memory_context') {
        const text =
          args.query === 'empty' || args.query === 'nothing'
            ? ''
            : `<agent_memory>\n- факт про ${args.query}\n</agent_memory>`
        return reply(id, { content: [{ type: 'text', text }] })
      }
      if (name === 'memory_search') {
        const text =
          args.query === 'nothing'
            ? 'ничего не найдено'
            : `[1] key=fact-1 | факт про ${args.query ?? ''}`
        return reply(id, { content: [{ type: 'text', text }] })
      }
      if (name === 'session_ingest') {
        const count = Array.isArray(args.messages) ? args.messages.length : 0
        return reply(id, { content: [{ type: 'text', text: `saved messages=${count}` }] })
      }
      if (name === 'broken') {
        return reply(id, { content: [{ type: 'text', text: '[Error] бум' }], isError: true })
      }
      return fail(id, `Unknown tool ${name}`)
    }
    default:
      return fail(id, `Unknown method ${method}`)
  }
})
