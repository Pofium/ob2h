/**
 * cordis-плагин DeepSeek Harness: OB2H как полноценный «провайдер памяти»
 * (аналог MemoryProvider в Hermes, см. `plugin/ob2h/`).
 *
 * Что делает, без инициативы модели:
 *   1. recall   — перед ходом подтягивает `memory_context` по тексту хода и
 *                 подмешивает блок `<agent_memory>` в батч сообщений
 *                 (штатный приём харнесса — так же делает dsh-time-context);
 *   2. capture  — каждый завершённый ход (`user/message` + `assistant/message`
 *                 до `turn/end`) уходит в `session_ingest` полным префиксом
 *                 сессии: сервер пишет только хвост (дедуп по session_id);
 *   3. заметка  — статичная секция системного промпта о подключённой памяти.
 *
 * Принципы: ноль внешних зависимостей (только `node:*`) — чтобы плагин
 * загружался независимо от версии и раскладки `@deepseek-ai/*` в профиле;
 * всё fail-open — память не имеет права ломать ход диалога.
 *
 * Ставится командой `ob2h agent install --agent dsh`.
 */

import { randomUUID } from 'node:crypto'
import { Ob2hMemory } from './lib/provider.js'
import { Ob2hRpc } from './lib/rpc.js'

/** Имя плагина в диагностике cordis-loader. */
export const name = 'ob2h'

/** Нужен реестр системного промпта (секция-заметка). Остальное — опционально. */
export const inject = ['systemPrompt']

/** Порядок секции: после persona-suffix (10200), ближе к концу промпта. */
const NOTE_ORDER = 10300

const NOTE_TEXT =
  'OB2H: подключена долговременная память. Релевантные воспоминания приходят блоком ' +
  '<agent_memory> в начале хода — опирайся на них. Явные операции — MCP-инструменты ' +
  'ob2h (memory_save, memory_search, knowledge_extract, graph_reason, dream_*).'

/** Значения по умолчанию; перекрываются блоком `config` в cordis.patch.yml. */
export const DEFAULTS = {
  binary: '',
  args: ['serve'],
  cwd: '',
  dataDir: '',
  env: {},
  source: 'dsh',
  author: 'dsh',
  note: true,
  capture: true,
  capturePrimaryOnly: true,
  recall: true,
  recallTimeoutMs: 2_500,
  maxTokens: 30,
  maxChars: 4_000,
  callTimeoutMs: 900_000,
  startupTimeoutMs: 30_000,
}

/** Собрать текст из блоков контента сообщения (`[{type:'text',text}]` или строка). */
export function blocksToText(content) {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content
    .map((block) =>
      block && block.type === 'text' && typeof block.text === 'string' ? block.text : '',
    )
    .filter(Boolean)
    .join('\n')
}

/** Глубокая заморозка — так же, как это делает createMessage в dsh-llm. */
function deepFreeze(value) {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    Object.freeze(value)
    for (const nested of Object.values(value)) deepFreeze(nested)
  }
  return value
}

/** Сообщение-снапшот с блоком памяти (форма как у dsh-time-context). */
export function snapshotMessage(text) {
  return deepFreeze({
    id: randomUUID(),
    role: 'user',
    content: [{ type: 'text', text }],
    source: {
      kind: name,
      form: 'snapshot',
      sections: [{ name, text }],
    },
  })
}

/** Текст пользовательского хода из батча pre-step (только настоящий ввод человека). */
export function queryFromMessages(messages) {
  if (!Array.isArray(messages)) return ''
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index]
    if (message?.role !== 'user') continue
    if (message.source?.kind && message.source.kind !== 'user') continue
    const text = blocksToText(message.content).trim()
    if (text) return text
  }
  return ''
}

/** Нормализовать конфиг плагина. */
export function resolveOptions(config = {}) {
  return { ...DEFAULTS, ...config }
}

/** Создать транспорт и провайдера памяти. */
export function createResources(options, logger) {
  const rpc = new Ob2hRpc({
    command: options.binary,
    args: options.args,
    cwd: options.cwd || undefined,
    env: options.dataDir
      ? { OB2H_DATA_DIR: options.dataDir, ...options.env }
      : { ...options.env },
    callTimeoutMs: options.callTimeoutMs,
    startupTimeoutMs: options.startupTimeoutMs,
    clientName: 'dsh-plugin-ob2h',
    logger,
  })
  const memory = new Ob2hMemory({
    rpc,
    source: options.source,
    author: options.author,
    prefetchTimeoutMs: options.recallTimeoutMs,
    maxTokens: options.maxTokens,
    maxChars: options.maxChars,
    logger,
  })
  return { rpc, memory }
}

/**
 * Подключить плагин к контексту cordis (вынесено из {@link apply} ради тестов:
 * сюда можно передать подставной ctx и подставные ресурсы).
 *
 * @param {any} ctx — контекст cordis (нужны systemPrompt.section, on, effect, get, logger)
 * @param {ReturnType<typeof resolveOptions>} options
 * @param {{rpc: Ob2hRpc, memory: Ob2hMemory}} resources
 */
export function install(ctx, options, resources) {
  const logger = ctx.logger ?? console
  const { rpc, memory } = resources

  /** Живые агенты: нужны, чтобы не писать в память ходы сабагентов. */
  const agents = typeof ctx.get === 'function' ? ctx.get('agents') : undefined
  const isPrimary = (agent) => {
    if (options.capturePrimaryOnly === false) return true
    try {
      if (
        !agent?.id ||
        typeof agents?.list !== 'function' ||
        typeof agents?.isOwnedBy !== 'function'
      ) {
        return true
      }
      return !agents
        .list()
        .some((candidate) => candidate.id !== agent.id && agents.isOwnedBy(agent.id, candidate.id))
    } catch {
      return true
    }
  }

  // 1. Заметка в системном промпте.
  if (options.note) {
    ctx.systemPrompt.section({ name: 'ob2h:note', order: NOTE_ORDER, text: NOTE_TEXT })
  }

  // 2. Recall перед ходом: блок памяти как user-снапшот в батче pre-step.
  if (options.recall) {
    ctx.on(
      'agent/pre-step',
      async (payload, next) => {
        const decision = await next()
        try {
          if (decision?.kind !== 'enter' || payload?.signal?.aborted) return decision
          // Ровно один раз на ход: на последующих шагах блок уже в истории.
          if (payload?.step !== 1) return decision
          const agent = payload.agent
          if (!isPrimary(agent)) return decision
          const query = queryFromMessages(decision.messages)
          if (!query) return decision
          const block = await memory.recall(
            agent?.session?.id ?? agent?.id ?? '',
            query,
            options.recallTimeoutMs,
          )
          if (!block) return decision
          return { ...decision, messages: [...decision.messages, snapshotMessage(block)] }
        } catch (error) {
          logger.debug?.(`ob2h: recall пропущен: ${String(error)}`)
          return decision
        }
      },
      { prepend: true },
    )
  }

  // 3. Автозахват: собираем ход по событиям сессии и пишем на turn/end.
  if (options.capture) {
    /** @type {Map<string, {user: string, assistant: string[]}>} */
    const turns = new Map()

    ctx.on('session/event', (session, event) => {
      try {
        const sessionId = session?.id
        if (!sessionId) return
        const data = event?.data ?? {}
        switch (event?.type) {
          case 'turn/start':
            turns.set(sessionId, { user: '', assistant: [] })
            return
          case 'user/message': {
            const state = turns.get(sessionId)
            // Только настоящий ввод человека: инъекции плагинов (kind != 'user') не пишем.
            if (!state || data.source?.kind !== 'user') return
            state.user = blocksToText(data.content)
            return
          }
          case 'assistant/message': {
            const state = turns.get(sessionId)
            if (!state) return
            const text = blocksToText(data.message?.content)
            if (text.trim()) state.assistant.push(text.trim())
            return
          }
          case 'turn/end': {
            const state = turns.get(sessionId)
            turns.delete(sessionId)
            if (!state) return
            if (!state.user.trim() && state.assistant.length === 0) return
            const agent = typeof agents?.get === 'function' ? agents.get(sessionId) : undefined
            if (!isPrimary(agent)) return
            memory.recordTurn(sessionId, state.user, state.assistant.join('\n\n'))
            return
          }
          default:
        }
      } catch (error) {
        logger.debug?.(`ob2h: захват хода пропущен: ${String(error)}`)
      }
    })

    ctx.effect(() => () => turns.clear(), 'ob2h: сброс состояния захвата')
  }

  // 4. Жизненный цикл дочернего процесса.
  ctx.effect(
    () => () => {
      void memory.close().catch(() => {})
    },
    'ob2h: остановка ob2h serve',
  )
  void rpc
    .start()
    .then(() => logger.info?.('ob2h: память подключена (долгоживущий ob2h serve)'))
    .catch((error) => logger.warn?.(`ob2h: процесс не поднялся сразу: ${String(error)}`))

  return { rpc, memory }
}

/**
 * Точка входа cordis.
 *
 * @param {any} ctx
 * @param {Partial<typeof DEFAULTS>} [config]
 */
export function apply(ctx, config = {}) {
  const options = resolveOptions(config)
  if (!options.binary) {
    ;(ctx.logger ?? console).warn?.('ob2h: не задан путь к бинарнику (config.binary) — плагин выключен')
    return undefined
  }
  return install(ctx, options, createResources(options, ctx.logger ?? console))
}
