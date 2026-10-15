import type { HistoryResponse, Snapshot, VocabItem } from './types';

type Invoke = <T>(name: string, args?: Record<string, unknown>) => Promise<T>;
declare global { interface Window { __TAURI__?: { core: { invoke: Invoke } } } }

let mock: Invoke | undefined;
async function invoke<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  if (window.__TAURI__) return window.__TAURI__.core.invoke<T>(name, args);
  if (import.meta.env.DEV) {
    if (!mock) mock = (await import('./dev/mock')).mockInvoke;
    return mock<T>(name, args);
  }
  throw new Error('NVG Voice native bridge is unavailable');
}

export const api = {
  snapshot: () => invoke<Snapshot>('get_snapshot'),
  history: () => invoke<HistoryResponse>('get_history'),
  set: (key: string, value: unknown) => invoke<Snapshot>('set_setting', { key, value }),
  vocabulary: (term: string, add: boolean) => invoke<string[]>('edit_vocabulary', { term, add }),
  removeAutoFix: (key: string) => invoke<void>('remove_auto_fix', { key }),
  getVocabulary: () => invoke<VocabItem[]>('get_vocabulary'),
  fix: (wrong: string, right: string) => invoke<void>('fix_word', { wrong, right }),
  key: (key: string) => invoke<void>('save_xai_key', { key }),
  microphones: () => invoke<string[]>('list_microphones'),
  copy: (text: string) => invoke<void>('copy_text', { text }),
  pause: (paused: boolean) => invoke<boolean>('set_paused', { paused }),
  openSettings: () => invoke<void>('open_settings'),
  closeFlyout: () => invoke<void>('close_flyout'),
  closeSettings: () => invoke<void>('close_settings'),
  logs: () => invoke<void>('open_log_folder'),
  quit: () => invoke<void>('quit_app'),
  ready: () => invoke<void>('report_ready'),
};
