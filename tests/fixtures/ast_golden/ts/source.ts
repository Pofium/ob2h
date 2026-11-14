import { Store } from './store';

/** Кеш токенов (эталон класса). */
export class TokenCache {
  refresh(): void {
    return;
  }
}

/** Загрузка настроек (эталон функции). */
export function loadSettings(path: string): void {
  return;
}

/** Пайплайн с прямым вызовом loadSettings — цель для tree-sitter (Фаза 70). */
export function runPipeline(path: string): void {
  loadSettings(path);
  return;
}
