import { useEffect, useRef, useState, type ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { api } from './bridge';
import type { Snapshot } from './types';
import { Icon, ModeSwitch, run } from './ui';
import './style.css';

function Row({ title, description, children, note }: { title: string; description?: string; children: ReactNode; note?: string }) {
  return <div className="srow"><div><div className="s-title">{title}</div>{description && <div className="s-desc">{description}</div>}{note && <div className="s-note">{note}</div>}</div><div className="s-control">{children}</div></div>;
}

const TABS = [['dictation', 'Dictation'], ['engine', 'API key'], ['text', 'Text cleanup'], ['system', 'System']] as const;
type Tab = typeof TABS[number][0];

const MOD_ORDER = ['Ctrl', 'Alt', 'Shift', 'Win'] as const;
const MODIFIER_KEYS: Record<string, string> = { Control: 'Ctrl', Alt: 'Alt', Shift: 'Shift', Meta: 'Win' };
const ADD_MODIFIER_MESSAGE = 'Add Ctrl, Alt, Shift or Win to the shortcut.';
const UNSUPPORTED_KEY_MESSAGE = 'That key is not supported. Use a letter, a digit, F1 to F12, Space, Enter or Tab.';

function mainKeyFromCode(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1];
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit) return digit[1];
  if (/^F([1-9]|1[0-2])$/.test(code)) return code;
  if (code === 'Space' || code === 'Enter' || code === 'Tab') return code;
  return null;
}

const isFunctionKey = (key: string) => /^F([1-9]|1[0-2])$/.test(key);
const orderedModifiers = (held: Set<string>): string[] => MOD_ORDER.filter(m => held.has(m));

function Settings() {
  const [tab, setTab] = useState<Tab>('dictation');
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const [settings, setSettings] = useState<Snapshot | null>(null);
  const [microphones, setMicrophones] = useState<string[]>([]);
  const [apiKey, setApiKey] = useState('');
  const [capturing, setCapturing] = useState(false);
  const [message, setMessage] = useState('');
  const [restart, setRestart] = useState<string[]>([]);
  useEffect(() => { void Promise.all([api.snapshot(), api.microphones()]).then(([s, m]) => { setSettings(s); setMicrophones(m); }).catch(e => setMessage(String(e))); }, []);
  useEffect(() => {
    if (capturing) return;
    const close = (e: globalThis.KeyboardEvent) => { if (e.key === 'Escape') { e.preventDefault(); void api.closeSettings(); } };
    document.addEventListener('keydown', close); return () => document.removeEventListener('keydown', close);
  }, [capturing]);
  const save = (key: string, value: unknown, needsRestart = false) => void run(async () => {
    setSettings(await api.set(key, value));
    if (needsRestart) setRestart(list => [...new Set([...list, key])]);
    setMessage(needsRestart ? 'Saved. Restart Sayso to apply.' : 'Saved');
  }, setMessage);
  useEffect(() => {
    if (!capturing) return;
    const held = new Set<string>();
    let mainPressed = false;
    let lastMods = new Set<string>();
    const finish = (value: string) => { setCapturing(false); save('hotkey', value, true); };
    const onKeyDown = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); setCapturing(false); return; }
      e.preventDefault(); e.stopPropagation();
      const mod = MODIFIER_KEYS[e.key];
      if (mod) { held.add(mod); lastMods = new Set(held); return; }
      mainPressed = true;
      const main = mainKeyFromCode(e.code);
      if (!main) { setMessage(UNSUPPORTED_KEY_MESSAGE); return; }
      const mods = orderedModifiers(held);
      if (mods.length === 0 && !isFunctionKey(main)) { setMessage(ADD_MODIFIER_MESSAGE); return; }
      finish([...mods, main].join('+'));
    };
    const onKeyUp = (e: globalThis.KeyboardEvent) => {
      const mod = MODIFIER_KEYS[e.key];
      if (!mod) return;
      held.delete(mod);
      if (held.size === 0 && !mainPressed) {
        const mods = orderedModifiers(lastMods);
        if (mods.length >= 2) finish(mods.join('+'));
        else setMessage(ADD_MODIFIER_MESSAGE);
      }
    };
    const onBlur = () => setCapturing(false);
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('keyup', onKeyUp);
    window.addEventListener('blur', onBlur);
    return () => { window.removeEventListener('keydown', onKeyDown); window.removeEventListener('keyup', onKeyUp); window.removeEventListener('blur', onBlur); };
  }, [capturing]);
  const boolRow = (title: string, description: string, field: keyof Snapshot, key: string, needsRestart = false) =>
    <Row title={title} description={description} note={restart.includes(key) ? 'Restart Sayso to apply' : undefined}>
      <label className="tg"><input type="checkbox" checked={Boolean(settings?.[field])} onChange={e => save(key, e.target.checked, needsRestart)}/><span className="tk"/><span className="sr-only">{title}</span></label>
    </Row>;
  return <main className="settings-window"><header className="settings-head"><span className="mark">●</span><span>Sayso</span><button className="settings-close" type="button" onClick={() => void api.closeSettings()} aria-label="Close settings"><Icon name="close" size={16}/></button></header>
    <nav className="fx-seg settings-tabs" role="tablist" aria-label="Settings sections">{TABS.map(([id, label], index) =>
      <button key={id} ref={node => { tabRefs.current[index] = node; }} type="button" role="tab" id={`tab-${id}`} aria-selected={tab === id} aria-controls="settings-panel" tabIndex={tab === id ? 0 : -1}
        onClick={() => setTab(id)} onKeyDown={e => { if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') { e.preventDefault(); const next = (index + (e.key === 'ArrowRight' ? 1 : TABS.length - 1)) % TABS.length; tabRefs.current[next]?.focus(); setTab(TABS[next][0]); } }}>{label}</button>)}</nav>
    <div className="settings-scroll" id="settings-panel" role="tabpanel" aria-labelledby={`tab-${tab}`} key={tab}>
      {tab === 'dictation' && <section aria-labelledby="dictation-title"><h2 id="dictation-title" className="sr-only">Dictation</h2>
        <Row title="Shortcut" description="Hold or press this key combination to dictate" note={restart.includes('hotkey') ? 'Restart Sayso to apply' : undefined}>
          <button type="button" className="keycap" onClick={() => { setMessage(''); setCapturing(true); }} onBlur={() => setCapturing(false)} aria-label={capturing ? 'Press a new shortcut' : `Change shortcut, currently ${settings?.hotkey ?? ''}`} aria-live="polite">{capturing ? 'Press keys…' : settings?.hotkey ?? 'Loading'}</button>
        </Row>
        {boolRow('Keep shortcut out of focused app', 'Prevent the key combination from reaching your other app', 'suppressHotkey', 'suppress_hotkey', true)}
        <Row title="Recording mode" description="Hold the shortcut or press once to start and stop"><ModeSwitch mode={settings?.recordingMode ?? 'hold'} onChange={m => save('recording_mode', m)}/></Row>
        <Row title="Microphone" description="Choose an input device" note={restart.includes('mic_device') ? 'Restart Sayso to apply' : undefined}>
          <select className="combo" aria-label="Microphone" value={settings?.micDevice ?? ''} onChange={e => save('mic_device', e.target.value, true)}><option value="">System default</option>{microphones.map(name => <option key={name} value={name}>{name}</option>)}</select>
        </Row>
      </section>}
      {tab === 'engine' && <section aria-labelledby="engine-title"><h2 id="engine-title" className="sr-only">API key</h2>
        <Row title="xAI API key" description={settings?.keySaved ? 'A key is saved. Enter a new one to replace it.' : 'No key saved. Dictation needs one.'}>
          <form className="key-form" onSubmit={e => { e.preventDefault(); void run(async () => { await api.key(apiKey); setApiKey(''); setSettings(s => s && ({ ...s, keySaved: true })); setMessage('Key saved'); }, setMessage); }}><label className="sr-only" htmlFor="api-key">New xAI API key</label><input id="api-key" type="password" autoComplete="off" value={apiKey} onChange={e => setApiKey(e.target.value)} placeholder="Enter new key" required/><button type="submit" className="accent-button">Save</button></form>
        </Row>
        <Row title="Engine" description="xAI Grok Voice. Audio is sent to xAI when you release the shortcut."><span className="s-desc">Only engine</span></Row>
      </section>}
      {tab === 'text' && <section aria-labelledby="text-title"><h2 id="text-title" className="sr-only">Text cleanup</h2>
        {boolRow('Remove fillers', 'Remove “um” and “uh” from dictations', 'removeFillers', 'remove_fillers')}
        {boolRow('Auto capitalise', 'Capitalise the first word of a sentence', 'autoCapitalize', 'auto_capitalize')}
        {boolRow('Code mode', 'Format spoken code punctuation', 'codeMode', 'code_mode')}
        {boolRow('Trailing space', 'Add one space after typed text', 'trailingSpace', 'trailing_space')}
        {boolRow('Learn my vocabulary automatically', 'Adds names from your folders and your own dictations as hints for the recogniser. Nothing is replaced in your text.', 'autoVocabulary', 'auto_vocabulary')}
      </section>}
      {tab === 'system' && <section aria-labelledby="system-title"><h2 id="system-title" className="sr-only">System</h2>
        {boolRow('Launch at startup', 'Start Sayso when you sign in', 'launchOnStartup', 'launch_on_startup')}
        <Row title="Sayso" description={`Version ${settings?.version ?? '…'}`}><button type="button" className="plain-button" onClick={() => void run(() => api.logs(), setMessage)}>Open log folder</button></Row>
      </section>}
    </div>{message && <div className="settings-message" role="status">{message}</div>}
  </main>;
}

createRoot(document.getElementById('root')!).render(<Settings/>);
