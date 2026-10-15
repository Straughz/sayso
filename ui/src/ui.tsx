import type { ReactNode } from 'react';
import { api } from './bridge';

const paths: Record<string, ReactNode> = {
  search: <><circle cx="11" cy="11" r="6"/><path d="M15.5 15.5 20 20"/></>,
  gear: <><circle cx="12" cy="12" r="3"/><path d="M19 15a2 2 0 0 0 0 2l1 1-2 2-1-1a2 2 0 0 0-2 0l-1 .4V22h-4v-1.6l-1-.4a2 2 0 0 0-2 0l-1 1-2-2 1-1a2 2 0 0 0 0-2l-.4-1H2v-4h1.6l.4-1a2 2 0 0 0 0-2L3 7l2-2 1 1a2 2 0 0 0 2 0l1-.4V3h4v1.6l1 .4a2 2 0 0 0 2 0l1-1 2 2-1 1a2 2 0 0 0 0 2l.4 1H22v4h-1.6z"/></>,
  back: <path d="m15 18-6-6 6-6"/>,
  close: <path d="M6 6 18 18M18 6 6 18"/>,
  plus: <path d="M12 5v14M5 12h14"/>,
  copy: <><rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></>,
  fix: <><path d="m4 20 13-13 3 3L7 23H4zM16 8l2-2 3 3-2 2"/><path d="M4 3v4M2 5h4"/></>,
  pause: <><path d="M8 5v14M16 5v14"/></>,
  play: <path d="m7 5 12 7-12 7z"/>,
  power: <><path d="M12 2v9"/><path d="M6.5 5.5a9 9 0 1 0 11 0"/></>,
  check: <path d="m4 12 5 5L20 6"/>,
};
export function Icon({ name, size = 18 }: { name: keyof typeof paths; size?: number }) {
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[name]}</svg>;
}
export function IconButton({ name, label, onClick, className = '' }: { name: string; label: string; onClick: () => void; className?: string }) {
  return <button type="button" className={`icon-button ${className}`} onClick={onClick} aria-label={label} title={label}><Icon name={name}/></button>;
}
export function ModeSwitch({ mode, onChange }: { mode: 'hold' | 'toggle'; onChange: (m: 'hold' | 'toggle') => void }) {
  return <div className="mode-switch" role="group" aria-label="Recording mode">{(['hold', 'toggle'] as const).map(m =>
    <button key={m} type="button" aria-pressed={mode === m} onClick={() => onChange(m)}>{m === 'hold' ? 'Hold' : 'Toggle'}</button>)}</div>;
}
export function useEscape(close: () => void) {
  // This small listener is local to each page. Nested forms handle Escape first.
  window.onkeydown = e => { if (e.key === 'Escape' && !e.defaultPrevented) { e.preventDefault(); close(); } };
}
export async function run(task: () => Promise<unknown>, onError: (message: string) => void) {
  try { await task(); } catch (e) { onError(String(e)); }
}
export const closeFlyout = () => { void api.closeFlyout(); };
