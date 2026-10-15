import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import { createRoot } from 'react-dom/client';
import { api } from './bridge';
import type { HistoryEntry, HistoryResponse, Snapshot, VocabItem } from './types';
import { Icon, IconButton, ModeSwitch, run } from './ui';
import './style.css';

type Pane = 'recent' | 'vocabulary' | 'stats' | 'history';
type Item = { type: 'day'; label: string } | { type: 'entry'; entry: HistoryEntry; id: number };
const dayLabel = (ts: number) => {
  const date = new Date(ts * 1000);
  const today = new Date();
  const yesterday = new Date(today); yesterday.setDate(today.getDate() - 1);
  if (date.toDateString() === today.toDateString()) return 'Today';
  if (date.toDateString() === yesterday.toDateString()) return 'Yesterday';
  return new Intl.DateTimeFormat(undefined, { weekday: 'long', month: 'short', day: 'numeric' }).format(date);
};
const timeLabel = (ts: number) => new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }).format(new Date(ts * 1000));
const duration = (s: number) => s >= 60 ? `${Math.floor(s / 60)}m ${Math.round(s % 60)}s` : `${Math.round(s)}s`;

function EntryRow({ entry, id, expanded, setExpanded, toast }: { entry: HistoryEntry; id: number; expanded: boolean; setExpanded: (n: number | null) => void; toast: (s: string) => void }) {
  const [wrong, setWrong] = useState('');
  const [right, setRight] = useState('');
  const save = (e: FormEvent) => {
    e.preventDefault();
    void run(async () => { await api.fix(wrong, right); toast('Correction saved'); setExpanded(null); setWrong(''); setRight(''); }, toast);
  };
  return <article className={`fx-row${expanded ? ' expanded' : ''}`}>
    <div className="meta"><time dateTime={new Date(entry.ts * 1000).toISOString()}>{timeLabel(entry.ts)}</time><span className="dur">{duration(entry.duration_s)}</span>{entry.undone && <span>Undone</span>}</div>
    <div className="row-text" title={entry.text}>{entry.text}</div>
    <div className="row-actions">
      <IconButton name="copy" label="Copy dictation" onClick={() => void run(async () => { await api.copy(entry.text); toast('Copied'); }, toast)}/>
      <button type="button" className="text-button" aria-expanded={expanded} aria-controls={`fix-${id}`} onClick={() => setExpanded(expanded ? null : id)}>Fix a word</button>
    </div>
    {expanded && <form id={`fix-${id}`} className="fix-form" onSubmit={save}>
      <label>Wrong word<input value={wrong} onChange={e => setWrong(e.target.value)} maxLength={80} autoFocus required/></label>
      <label>Right word<input value={right} onChange={e => setRight(e.target.value)} maxLength={80} required/></label>
      <button type="submit" className="accent-button">Save correction</button>
    </form>}
  </article>;
}

function VirtualHistory({ entries, expanded, setExpanded, toast }: { entries: HistoryEntry[]; expanded: number | null; setExpanded: (n: number | null) => void; toast: (s: string) => void }) {
  const [top, setTop] = useState(0);
  const [height, setHeight] = useState(360);
  const scroller = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const node = scroller.current; if (!node) return;
    const observer = new ResizeObserver(() => setHeight(node.clientHeight));
    observer.observe(node); setHeight(node.clientHeight);
    return () => observer.disconnect();
  }, []);
  const items = useMemo(() => {
    const result: Item[] = []; let previous = '';
    entries.forEach((entry, id) => {
      const label = dayLabel(entry.ts);
      if (label !== previous) { result.push({ type: 'day', label }); previous = label; }
      result.push({ type: 'entry', entry, id });
    });
    return result;
  }, [entries]);
  const offsets = useMemo(() => {
    const values = [0];
    for (const item of items) values.push(values.at(-1)! + (item.type === 'day' ? 32 : item.id === expanded ? 222 : 108));
    return values;
  }, [items, expanded]);
  const first = Math.max(0, offsets.findIndex((_, i) => i < items.length && offsets[i + 1] >= top) - 3);
  let last = first;
  while (last < items.length && offsets[last] < top + height + 320) last++;
  const rendered = items.slice(first, last);
  return <div className="virtual-scroll" ref={scroller} onScroll={e => setTop(e.currentTarget.scrollTop)}>
    <div style={{ height: offsets[first] }}/>
    {rendered.map((item, j) => item.type === 'day' ? <div className="fx-day" key={`d-${first + j}`}>{item.label}</div> :
      <EntryRow key={`e-${item.id}`} entry={item.entry} id={item.id} expanded={expanded === item.id} setExpanded={setExpanded} toast={toast}/>)}
    <div style={{ height: offsets[items.length] - offsets[last] }}/>
  </div>;
}

function Flyout() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [history, setHistory] = useState<HistoryResponse | null>(null);
  const [pane, setPane] = useState<Pane>('recent');
  const [previous, setPrevious] = useState<Pane>('recent');
  const [query, setQuery] = useState('');
  const [search, setSearch] = useState('');
  const [term, setTerm] = useState('');
  const [vocab, setVocab] = useState<VocabItem[]>([]);
  const [expanded, setExpanded] = useState<number | null>(null);
  const [message, setMessage] = useState('');
  const [visible, setVisible] = useState(document.visibilityState === 'visible');
  const tabs = useRef<Array<HTMLButtonElement | null>>([]);
  const searchInput = useRef<HTMLInputElement>(null);
  const lastStatus = useRef('');
  const toast = (text: string) => setMessage(text);
  useEffect(() => { void Promise.all([api.snapshot(), api.history(), api.getVocabulary(), api.ready()]).then(([s, h, v]) => { lastStatus.current = s.status; setSnapshot(s); setHistory(h); setVocab(v); }).catch(e => toast(String(e))); }, []);
  useEffect(() => {
    if (!visible || pane !== 'vocabulary') return;
    void api.getVocabulary().then(setVocab).catch(() => {});
  }, [visible, pane]);
  useEffect(() => {
    if (!window.__TAURI__) return;
    let active = true;
    let unlisten: (() => void) | undefined;
    void import('@tauri-apps/api/event').then(({ listen }) => listen<string>('voice-status', event => {
      const previous = lastStatus.current;
      lastStatus.current = event.payload;
      setSnapshot(s => s && ({ ...s, status: event.payload }));
      if (event.payload === 'Ready' && previous === 'Processing') void api.history().then(setHistory);
    })).then(stop => { if (active) unlisten = stop; else stop(); });
    return () => { active = false; unlisten?.(); };
  }, []);
  useEffect(() => {
    const onVisibility = () => setVisible(document.visibilityState === 'visible');
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  }, []);
  useEffect(() => {
    if (!visible) return;
    const timer = window.setInterval(() => { void api.snapshot().then(s => { lastStatus.current = s.status; setSnapshot(s); }).catch(() => {}); }, 1000);
    return () => window.clearInterval(timer);
  }, [visible]);
  useEffect(() => { const timer = window.setTimeout(() => setSearch(query.trim().toLocaleLowerCase()), 250); return () => window.clearTimeout(timer); }, [query]);
  useEffect(() => { if (!message) return; const timer = window.setTimeout(() => setMessage(''), 2500); return () => window.clearTimeout(timer); }, [message]);
  useEffect(() => {
    const close = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.preventDefault(); if (expanded !== null) setExpanded(null); else if (pane === 'history') setPane(previous); else void api.closeFlyout(); } };
    document.addEventListener('keydown', close); return () => document.removeEventListener('keydown', close);
  }, [pane, previous, expanded]);
  const show = (next: Pane) => { if (next === 'history') { setPrevious(pane); window.setTimeout(() => searchInput.current?.focus(), 0); } setPane(next); setExpanded(null); };
  const filtered = useMemo(() => history?.entries.filter(e => e.text.toLocaleLowerCase().includes(search)) ?? [], [history, search]);
  const mode = (next: 'hold' | 'toggle') => void run(async () => setSnapshot(await api.set('recording_mode', next)), toast);
  const addTerm = (e: FormEvent) => { e.preventDefault(); void run(async () => {
    const hotWords = await api.vocabulary(term, true); setSnapshot(s => s && ({ ...s, hotWords }));
    setVocab(await api.getVocabulary()); setTerm(''); toast('Vocabulary updated');
  }, toast); };
  const removeTerm = (item: VocabItem) => void run(async () => {
    if (item.source === 'auto-fix' && item.key) {
      await api.removeAutoFix(item.key);
    } else {
      const hotWords = await api.vocabulary(item.term, false); setSnapshot(s => s && ({ ...s, hotWords }));
    }
    setVocab(await api.getVocabulary());
  }, toast);
  const pause = () => void run(async () => { const paused = await api.pause(!snapshot?.paused); setSnapshot(s => s && ({ ...s, paused, status: paused ? 'Paused' : 'Ready' })); }, toast);
  const stats = history?.stats;
  return <main className="flyout" data-visible={visible} aria-label="Sayso flyout">
    <header className="fx-head">
      <div className="fx-status"><span className={`fx-dot${snapshot?.status === 'Ready' ? '' : ' subdued'}`}/><span className="status-text">{snapshot?.status ?? 'Loading'}<span>. {snapshot?.status === 'Missing API key' ? 'Open Settings to add one' : `${snapshot?.recordingMode === 'toggle' ? 'Press' : 'Hold'} ${snapshot?.hotkey ?? 'shortcut'} to dictate`}</span></span></div>
      <div className="fx-headrow"><div className="meter" aria-hidden="true">{Array.from({ length: 26 }, (_, i) => <i key={i} className={i > 13 && i < 22 ? 'on' : ''} style={{ height: `${5 + Math.round(Math.abs(Math.sin(i * 1.7)) * 11)}px` }}/>)}</div><span className="meter-label">Mic level</span><ModeSwitch mode={snapshot?.recordingMode ?? 'hold'} onChange={mode}/></div>
    </header>
    <nav className="fx-seg" role="tablist" aria-label="Panel sections">{(['recent', 'vocabulary', 'stats'] as const).map((tab, index) =>
      <button key={tab} ref={node => { tabs.current[index] = node; }} type="button" role="tab" aria-selected={pane === tab} tabIndex={pane === tab || (pane === 'history' && tab === previous) ? 0 : -1}
        onClick={() => show(tab)} onKeyDown={e => { if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') { e.preventDefault(); const next = (index + (e.key === 'ArrowRight' ? 1 : 2)) % 3; tabs.current[next]?.focus(); show((['recent', 'vocabulary', 'stats'] as const)[next]); } }}>{tab[0].toUpperCase() + tab.slice(1)}</button>)}</nav>
    <section className="fx-body" role="tabpanel" aria-label={pane}>
      {pane === 'recent' && <div className="fx-content">{history?.entries.length ? <><div className="fx-list-title">Recent dictations</div>{history.entries.slice(0, 8).map((entry, id) => <EntryRow key={id} entry={entry} id={id} expanded={expanded === id} setExpanded={setExpanded} toast={toast}/>)}</> : <p className="empty">No dictations yet. Use your shortcut and speak.</p>}</div>}
      {pane === 'vocabulary' && <div className="fx-content"><p className="fx-list-title">{vocab.filter(item => item.source === 'manual').length} of 100 terms</p><form className="fx-add" onSubmit={addTerm}><label className="sr-only" htmlFor="term">Add a vocabulary term</label><input id="term" value={term} onChange={e => setTerm(e.target.value)} maxLength={50} placeholder="Name or term" required/><button className="accent-button" type="submit" aria-label="Add term"><Icon name="plus"/></button></form><div className="fx-chips">{vocab.map(item => <span key={item.key ?? item.term.toLowerCase()} className="fx-chip">{item.term}{item.source !== 'manual' && <span className="fx-tag">auto</span>}<button type="button" onClick={() => removeTerm(item)} aria-label={`Remove ${item.term}`}><Icon name="close" size={13}/></button></span>)}</div></div>}
      {pane === 'stats' && <div className="fx-content"><div className="fx-statgrid"><div className="fx-stat"><div className="n">{(stats?.total_words ?? 0).toLocaleString()}</div><div className="l">words</div></div><div className="fx-stat"><div className="n">{Math.floor((stats?.speaking_time_s ?? 0) / 3600)}h {Math.floor(((stats?.speaking_time_s ?? 0) % 3600) / 60)}m</div><div className="l">spoken</div></div><div className="fx-stat"><div className="n">{(stats?.entry_count ?? 0).toLocaleString()}</div><div className="l">dictations</div></div><div className="fx-stat"><div className="n">{Math.round(stats?.avg_wpm ?? 0)}</div><div className="l">wpm</div></div></div><div className="fx-week">This week: <b>{(stats?.week_words ?? 0).toLocaleString()} words</b> · <b>{(stats?.week_dictations ?? 0).toLocaleString()} dictations</b></div></div>}
      {pane === 'history' && <div className="history-pane"><div className="fx-history-head"><IconButton name="back" label="Back to previous section" onClick={() => show(previous)}/><strong>All history</strong></div><div className="fx-search"><Icon name="search" size={16}/><label className="sr-only" htmlFor="history-search">Search dictations</label><input id="history-search" ref={searchInput} type="search" value={query} onChange={e => setQuery(e.target.value)} placeholder="Search dictations"/></div>{filtered.length ? <VirtualHistory entries={filtered} expanded={expanded} setExpanded={setExpanded} toast={toast}/> : <p className="empty">No matching dictations.</p>}</div>}
    </section>
    <footer className="fx-foot"><IconButton name="search" label="Search history" onClick={() => show('history')}/><IconButton name="gear" label="Open settings" onClick={() => void run(() => api.openSettings(), toast)}/><IconButton name={snapshot?.paused ? 'play' : 'pause'} label={snapshot?.paused ? 'Resume dictation' : 'Pause dictation'} onClick={pause}/><IconButton name="power" label="Quit Sayso" onClick={() => void api.quit()}/><span className="engine"><b>xAI Grok Voice</b>{snapshot?.micDevice || 'System default'}</span></footer>
    {message && <div className="fx-toast" role="status">{message}</div>}
  </main>;
}

createRoot(document.getElementById('root')!).render(<Flyout/>);
