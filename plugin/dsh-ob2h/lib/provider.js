/**
 * Логика MemoryProvider поверх {@link Ob2hRpc} — порт `plugin/ob2h/__init__.py`:
 *
 * - автозахват: каждый ход (user+assistant) уходит в `session_ingest` полным
 *   префиксом сессии — сервер пишет только хвост (дедуп по session_id/позиции);
 * - prefetch: заранее (в фоне) запрашивается `memory_context` и складывается в кэш
 *   сессии; к моменту сборки запроса блок `<agent_memory>` забирается синхронно;
 * - все ошибки fail-open: память никогда не ломает ход диалога.
 */

const TAG = 'agent_memory'

export class Ob2hMemory {
  /**
   * @param {object} options
   * @param {import('./rpc.js').Ob2hRpc} options.rpc
   * @param {string} [options.source] — метка источника сессии (пишется в daily-лог)
   * @param {string} [options.author] — turn_author (meta.author записей, Ф25.2)
   * @param {number} [options.prefetchTimeoutMs]
   * @param {number} [options.writeTimeoutMs]
   * @param {number} [options.maxChars] — бюджет символов блока (OB2H_PREFETCH_MAX_CHARS)
   * @param {{debug?: Function, warn?: Function, info?: Function}} [options.logger]
   */
  constructor(options) {
    this.rpc = options.rpc
    this.source = options.source ?? 'dsh'
    this.author = options.author ?? 'dsh'
    this.prefetchTimeoutMs = options.prefetchTimeoutMs ?? 10_000
    this.writeTimeoutMs = options.writeTimeoutMs ?? 120_000
    this.maxTokens = options.maxTokens ?? 30
    this.maxChars = options.maxChars ?? 0
    this.logger = options.logger ?? {}

    /** @type {Map<string, Array<{role: string, content: string}>>} */
    this.accumulated = new Map()
    /** @type {Map<string, {text: string, hits: number, at: number}>} */
    this.prefetchCache = new Map()
    /** @type {Map<string, string>} */
    this.pendingQueries = new Map()
    /** @type {Map<string, Array<{role: string, content: string}>>} */
    this.writeQueue = new Map()
    this.writing = false
    this.closed = false
    this.stats = { turns: 0, ingests: 0, prefetches: 0, hits: 0, timeouts: 0, errors: 0 }
  }

  /** Запомнить ход и поставить его в очередь записи (не блокирует вызывающего). */
  recordTurn(sessionId, userContent, assistantContent) {
    if (this.closed) return
    const user = (userContent ?? '').trim()
    const assistant = (assistantContent ?? '').trim()
    if (!user && !assistant) return
    const sid = sessionId || 'default'
    const messages = this.accumulated.get(sid) ?? []
    messages.push({ role: 'user', content: userContent ?? '' })
    messages.push({ role: 'assistant', content: assistantContent ?? '' })
    this.accumulated.set(sid, messages)
    this.stats.turns += 1

    // Полный префикс: сервер сам отбросит уже принятые сообщения.
    this.writeQueue.set(sid, structuredClone(messages))
    void this.#drainWrites()
  }

  async #drainWrites() {
    if (this.writing) return
    this.writing = true
    try {
      while (this.writeQueue.size > 0) {
        const [sid, messages] = this.writeQueue.entries().next().value
        this.writeQueue.delete(sid)
        try {
          await this.rpc.ensure()
          await this.rpc.toolCall(
            'session_ingest',
            {
              messages,
              source: this.source,
              session_id: sid,
              author: this.author,
            },
            this.writeTimeoutMs,
          )
          this.stats.ingests += 1
        } catch (error) {
          this.stats.errors += 1
          this.logger.warn?.(`ob2h: запись хода не удалась: ${error.message}`)
        }
      }
    } finally {
      this.writing = false
    }
  }

  /** Дождаться, пока очередь записи опустеет (для тестов и корректной остановки). */
  async flush() {
    while (this.writing || this.writeQueue.size > 0) {
      await new Promise((resolve) => setTimeout(resolve, 5))
    }
  }

  /**
   * Синхронный (в смысле API) recall для текущего хода: если в кэше уже лежит
   * готовый блок под этот же запрос — отдаём его, иначе ждём выборку с таймаутом.
   * Всегда fail-open: при ошибке/таймауте возвращает ''.
   */
  async recall(sessionId, query, timeoutMs) {
    const sid = sessionId || 'default'
    const text = (query ?? '').trim()
    if (!text) return ''
    const cached = this.prefetchCache.get(sid)
    if (cached && cached.query === text) {
      this.prefetchCache.delete(sid)
      return cached.text
    }
    const limit = timeoutMs ?? this.prefetchTimeoutMs
    const fetched = await Promise.race([
      this.#fetch(sid, text),
      new Promise((resolve) => {
        const timer = setTimeout(() => resolve(null), limit)
        timer.unref?.()
      }),
    ])
    if (!fetched) {
      this.stats.timeouts += 1
      this.logger.debug?.(`ob2h: recall не успел за ${limit} мс`)
      return ''
    }
    return fetched.text
  }

  /**
   * Запустить фоновый prefetch под будущий ход. Идемпотентно на сессию:
   * пока запрос в полёте, повторный вызов обновляет только query.
   */
  queuePrefetch(sessionId, query) {
    if (this.closed) return
    const sid = sessionId || 'default'
    const text = (query ?? '').trim()
    if (!text) return
    this.pendingQueries.set(sid, text)
    if (this.inflight?.has(sid)) return
    this.inflight ??= new Set()
    this.inflight.add(sid)
    void this.#runPrefetch(sid).finally(() => this.inflight.delete(sid))
  }

  async #runPrefetch(sid) {
    // Пока идёт запрос, query мог обновиться — берём самый свежий.
    while (this.pendingQueries.has(sid)) {
      const query = this.pendingQueries.get(sid)
      this.pendingQueries.delete(sid)
      const result = await this.#fetch(sid, query)
      this.prefetchCache.set(sid, result)
    }
  }

  async #fetch(sid, query) {
    const params = { query, max_tokens: this.maxTokens }
    if (this.author) params.author = this.author
    if (this.maxChars > 0) params.max_chars = this.maxChars
    void sid
    try {
      await this.rpc.ensure()
      let block = (await this.rpc.toolCall('memory_context', params, this.prefetchTimeoutMs))?.trim() ?? ''
      if (!block) {
        const hits = (await this.rpc.toolCall(
          'memory_search',
          { query, limit: 8 },
          this.prefetchTimeoutMs,
        ))?.trim() ?? ''
        if (hits && !/ничего не найдено/i.test(hits)) {
          block = `<${TAG}>\n${hits}\n</${TAG}>`
        }
      }
      this.stats.prefetches += 1
      return { text: block, hits: countRecords(block), query, at: Date.now() }
    } catch (error) {
      this.stats.errors += 1
      this.logger.debug?.(`ob2h: prefetch не удался: ${error.message}`)
      return { text: '', hits: 0, query, at: Date.now() }
    }
  }

  /** Синхронно забрать готовый блок памяти для хода (пустая строка, если нет). */
  takePrefetch(sessionId) {
    const sid = sessionId || 'default'
    const cached = this.prefetchCache.get(sid)
    if (!cached) return ''
    this.prefetchCache.delete(sid)
    this.stats.hits += cached.hits
    return cached.text
  }

  /** Короткая статичная заметка для системного промпта (в духе Hermes-провайдера). */
  systemPromptNote() {
    return (
      'Долговременная память ob2h подключена: релевантные воспоминания автоматически ' +
      'всплывают блоками <agent_memory> перед ходом — опирайся на них. ' +
      'Явные операции — инструменты ob2h (memory_save, knowledge_extract, graph_reason, dream_*).'
    )
  }

  async close() {
    this.closed = true
    try {
      await this.flush()
    } catch {
      /* очередь уже не важна */
    }
    await this.rpc.stop()
  }
}

/** Число «содержательных» строк блока (без служебных XML-тегов) — для счётчика recall. */
export function countRecords(block) {
  if (!block) return 0
  return block
    .split('\n')
    .filter((line) => line.trim() && !line.trim().startsWith('<')).length
}
