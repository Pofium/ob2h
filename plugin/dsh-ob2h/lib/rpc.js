/**
 * JSON-RPC 2.0-клиент поверх stdio-процесса `ob2h serve`.
 *
 * Порт `plugin/ob2h/_rpc.py` (Hermes MemoryProvider) на ESM-JS без зависимостей:
 * один долгоживущий дочерний процесс, построчный JSON-RPC, ожидание ответов по id,
 * рестарт с backoff, мягкое завершение (закрытие stdin = EOF).
 */

import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'

/** Общая ошибка вызова (таймаут, JSON-RPC error). */
export class RpcError extends Error {}

/** Процесс ob2h не запущен / умер. */
export class RpcDead extends RpcError {}

export const PROTOCOL_VERSION = '2024-11-05'

export class Ob2hRpc {
  /**
   * @param {object} options
   * @param {string} options.command — путь к ob2h(.exe)
   * @param {string[]} [options.args] — аргументы (по умолчанию ['serve'])
   * @param {Record<string,string>} [options.env] — доп. переменные окружения дочернего процесса
   * @param {string} [options.cwd] — рабочий каталог (пин data_dir)
   * @param {number} [options.callTimeoutMs]
   * @param {number} [options.startupTimeoutMs]
   * @param {number} [options.restartBackoffMs]
   * @param {string} [options.clientName]
   * @param {{debug?: Function, warn?: Function, info?: Function}} [options.logger]
   */
  constructor(options) {
    this.command = options.command
    this.args = options.args ?? ['serve']
    this.env = options.env ?? {}
    this.cwd = options.cwd
    this.callTimeoutMs = options.callTimeoutMs ?? 900_000
    this.startupTimeoutMs = options.startupTimeoutMs ?? 30_000
    this.restartBackoffMs = options.restartBackoffMs ?? 5_000
    this.clientName = options.clientName ?? 'dsh-ob2h-plugin'
    this.logger = options.logger ?? {}
    /** @type {import('node:child_process').ChildProcess | null} */
    this.proc = null
    /** @type {Map<number, {resolve: Function, reject: Function, timer: NodeJS.Timeout}>} */
    this.pending = new Map()
    this.nextId = 0
    this.lastStart = 0
    this.starting = null
  }

  alive() {
    return Boolean(this.proc) && this.proc.exitCode === null && !this.proc.killed
  }

  /** Поднять процесс и выполнить MCP-handshake. Идемпотентно, с backoff на рестарт. */
  async start() {
    if (this.alive()) return
    if (this.starting) return this.starting
    this.starting = this.#start().finally(() => {
      this.starting = null
    })
    return this.starting
  }

  async #start() {
    const now = Date.now()
    if (now - this.lastStart < this.restartBackoffMs) {
      throw new RpcDead(
        `ob2h: перезапуск чаще раза в ${this.restartBackoffMs} мс (backoff)`,
      )
    }
    this.lastStart = now

    const proc = spawn(this.command, this.args, {
      cwd: this.cwd,
      env: { ...process.env, ...this.env },
      stdio: ['pipe', 'pipe', 'ignore'],
      windowsHide: true,
    })
    this.proc = proc

    proc.on('error', (error) => {
      this.logger.warn?.(`ob2h: процесс не запустился: ${error.message}`)
      this.#failAll(new RpcDead(`ob2h: процесс не запустился: ${error.message}`))
    })
    proc.on('close', () => {
      this.#failAll(new RpcDead('ob2h: процесс завершился'))
    })

    const lines = createInterface({ input: proc.stdout })
    lines.on('line', (line) => this.#onLine(line))

    await this.#handshake()
    this.logger.info?.(`ob2h: процесс запущен (${this.command} ${this.args.join(' ')})`)
  }

  /** Поднять процесс, если он не жив. */
  async ensure() {
    if (!this.alive()) await this.start()
  }

  #onLine(line) {
    const trimmed = line.trim()
    if (!trimmed) return
    let message
    try {
      message = JSON.parse(trimmed)
    } catch {
      return
    }
    const id = message.id
    if (id === undefined || id === null) return // уведомления сервера не ожидаем
    const slot = this.pending.get(id)
    if (!slot) return
    this.pending.delete(id)
    clearTimeout(slot.timer)
    slot.resolve(message)
  }

  #failAll(error) {
    for (const [, slot] of this.pending) {
      clearTimeout(slot.timer)
      slot.reject(error)
    }
    this.pending.clear()
  }

  async #handshake() {
    await this.call(
      'initialize',
      {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: this.clientName, version: '0.1.0' },
      },
      this.startupTimeoutMs,
    )
    // Уведомление: сервер молча принимает его.
    this.notify('notifications/initialized')
  }

  /** Отправить уведомление (без id, без ответа). */
  notify(method, params) {
    const payload = { jsonrpc: '2.0', method }
    if (params !== undefined) payload.params = params
    this.#write(payload)
  }

  /** Вызвать метод и дождаться result. */
  async call(method, params, timeoutMs) {
    const limit = timeoutMs ?? this.callTimeoutMs
    const id = ++this.nextId
    const payload = { jsonrpc: '2.0', id, method }
    if (params !== undefined) payload.params = params

    const response = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new RpcError(`ob2h: таймаут ${method} (${limit} мс)`))
      }, limit)
      timer.unref?.()
      this.pending.set(id, { resolve, reject, timer })
    })

    try {
      this.#write(payload)
    } catch (error) {
      const slot = this.pending.get(id)
      if (slot) {
        this.pending.delete(id)
        clearTimeout(slot.timer)
      }
      throw error
    }

    const message = await response
    if (message.error) {
      throw new RpcError(`ob2h: ${method}: ${message.error.message ?? 'ошибка'}`)
    }
    return message.result
  }

  #write(payload) {
    const proc = this.proc
    if (!proc || !this.alive() || !proc.stdin || proc.stdin.destroyed) {
      throw new RpcDead('ob2h: процесс не запущен')
    }
    try {
      proc.stdin.write(`${JSON.stringify(payload)}\n`)
    } catch (error) {
      throw new RpcDead(`ob2h: процесс мёртв (запись в stdin): ${error.message}`)
    }
  }

  /** Список инструментов сервера (tools/list). */
  async toolsList(timeoutMs) {
    const result = await this.call('tools/list', {}, timeoutMs)
    return result?.tools ?? []
  }

  /** Вызвать MCP-инструмент и вернуть склеенный текст результата. */
  async toolCall(name, args, timeoutMs) {
    const result = await this.call(
      'tools/call',
      { name, arguments: args ?? {} },
      timeoutMs,
    )
    const content = result?.content ?? []
    return content
      .map((part) => (part && typeof part.text === 'string' ? part.text : ''))
      .filter(Boolean)
      .join('\n')
  }

  /** Проверка живости (метод ping есть у ob2h, как у долгоживущего MCP-сервера). */
  async ping(timeoutMs = 5_000) {
    try {
      await this.call('ping', {}, timeoutMs)
      return true
    } catch {
      return false
    }
  }

  /** Мягко остановить процесс: закрыть stdin (EOF) и подождать. */
  async stop() {
    const proc = this.proc
    this.proc = null
    this.#failAll(new RpcDead('ob2h: клиент остановлен'))
    if (!proc || proc.exitCode !== null) return

    const exited = new Promise((resolve) => proc.once('close', resolve))
    try {
      proc.stdin?.end()
    } catch {
      /* уже закрыт */
    }
    const timer = setTimeout(() => {
      try {
        proc.kill()
      } catch {
        /* уже мёртв */
      }
    }, 5_000)
    timer.unref?.()
    await exited
    clearTimeout(timer)
  }
}
