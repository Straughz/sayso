import type { HistoryResponse, Snapshot, VocabItem } from '../types';

const snapshot: Snapshot = {
  status: 'Ready', hotkey: 'Ctrl+Space', suppressHotkey: true, recordingMode: 'hold',
  micDevice: 'fifine Microphone', keySaved: true, removeFillers: true,
  autoCapitalize: true, codeMode: false, trailingSpace: true, launchOnStartup: true,
  autoVocabulary: true,
  hotWords: ['ADE', 'Anthropic', 'Astra', 'Claude Code', 'Codex', 'Cokpit', 'Deepgram', 'Fable', 'Tauri', 'xAI'],
  paused: false, version: '0.1.0',
};
let autoTerms: VocabItem[] = [
  { term: 'NVG Talk', source: 'workspace', status: 'trial' },
  { term: 'Plexus', source: 'workspace', status: 'active' },
  { term: 'Whisperflow', source: 'review', status: 'active' },
  { term: 'satterbar → status bar', source: 'auto-fix', status: 'active', key: 'satterbar' },
];
const phrases = [
  'Okay, the new version is installed. Check the scroll bar on the settings page, it jumps when I use the wheel over a row.',
  'Go ahead and begin on the design pass. I want a full redesign of the dashboard and settings, and the pill should reflect my actual voice.',
  'Hello, testing one, two.',
  'Add those names to the vocabulary, then look at why long dictations are not showing up in the Cokpit pane.',
  'Merge the branch, build the installer and install it. Keep the previous installer as a fallback.',
  'Ask Codex to review the routing change before we ship it.',
];
const now = Date.now() / 1000;
const entries = Array.from({ length: 120 }, (_, i) => ({
  ts: now - i * 1200 - Math.floor(i / 12) * 36000,
  text: phrases[i % phrases.length], duration_s: [17, 64, 1, 21, 38, 12][i % 6],
  words: phrases[i % phrases.length].split(/\s+/).length, undone: false,
}));
const history: HistoryResponse = { entries, stats: {
  total_words: 33221, speaking_time_s: 15060, entry_count: 1010, avg_wpm: 132,
  week_words: 2140, week_dictations: 61,
} };

export async function mockInvoke<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  switch (name) {
    case 'get_snapshot': return { ...snapshot, hotWords: [...snapshot.hotWords] } as T;
    case 'get_history': return history as T;
    case 'get_vocabulary': return [
      ...snapshot.hotWords.map(term => ({ term, source: 'manual' as const, status: 'active' as const })),
      ...autoTerms,
    ] as T;
    case 'set_setting': {
      const field = ({ recording_mode: 'recordingMode', hotkey: 'hotkey', mic_device: 'micDevice',
        suppress_hotkey: 'suppressHotkey', remove_fillers: 'removeFillers', auto_capitalize: 'autoCapitalize',
        code_mode: 'codeMode', trailing_space: 'trailingSpace', launch_on_startup: 'launchOnStartup',
        auto_vocabulary: 'autoVocabulary',
      } as Record<string, keyof Snapshot>)[String(args?.key)];
      if (field) Object.assign(snapshot, { [field]: args?.value });
      return { ...snapshot } as T;
    }
    case 'edit_vocabulary': {
      const term = String(args?.term ?? '').trim();
      const key = term.toLowerCase();
      if (args?.add) {
        if (!snapshot.hotWords.some(w => w.toLowerCase() === key)) snapshot.hotWords = [...snapshot.hotWords, term];
        autoTerms = autoTerms.filter(t => t.term.toLowerCase() !== key);
      } else if (snapshot.hotWords.some(w => w.toLowerCase() === key)) {
        snapshot.hotWords = snapshot.hotWords.filter(w => w.toLowerCase() !== key);
      } else {
        autoTerms = autoTerms.filter(t => t.term.toLowerCase() !== key);
      }
      return [...snapshot.hotWords] as T;
    }
    case 'remove_auto_fix': {
      const key = String(args?.key ?? '').toLowerCase();
      autoTerms = autoTerms.filter(t => !(t.source === 'auto-fix' && t.key === key));
      return undefined as T;
    }
    case 'set_paused': snapshot.paused = Boolean(args?.paused); snapshot.status = snapshot.paused ? 'Paused' : 'Ready'; return snapshot.paused as T;
    case 'list_microphones': return ['fifine Microphone', 'Microphone Array'] as T;
    case 'save_xai_key': snapshot.keySaved = true; return undefined as T;
    default: return undefined as T;
  }
}
