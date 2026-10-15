export type Snapshot = {
  status: string; hotkey: string; suppressHotkey: boolean; recordingMode: 'hold' | 'toggle';
  micDevice: string; keySaved: boolean; removeFillers: boolean; autoCapitalize: boolean;
  codeMode: boolean; trailingSpace: boolean; launchOnStartup: boolean;
  autoVocabulary: boolean; hotWords: string[]; paused: boolean; version: string;
};
export type VocabItem = { term: string; source: 'manual' | 'workspace' | 'review' | 'auto-fix'; status: 'trial' | 'active' | 'retired'; key?: string };
export type HistoryEntry = { ts: number; text: string; duration_s: number; words: number; undone: boolean };
export type HistoryStats = { total_words: number; speaking_time_s: number; entry_count: number; avg_wpm: number; week_words: number; week_dictations: number };
export type HistoryResponse = { entries: HistoryEntry[]; stats: HistoryStats };
