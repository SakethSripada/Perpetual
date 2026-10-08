/*
 * Agent activity primitives: pixel loader, shimmer text, tool-run trace, and
 * file chips. Adapted from Beautiful UI (https://www.beautifului.dev),
 * MIT License, Copyright (c) 2026 Shane Levine. See licenses/beautiful-ui.txt.
 */
import {
  useEffect,
  useState,
  type ReactNode,
} from 'react';
import type { AgentThreadEvent } from './types';


const cn = (...values: (string | false | null | undefined)[]) => values.filter(Boolean).join(' ');
function shellCommand(command: string) {
  const match = /^\s*"?[^"\s]*?\b(?:powershell|pwsh|cmd|bash|sh|zsh)(?:\.exe)?"?\s+(?:-NoProfile\s+|-NoLogo\s+)*(?:-Command|-c|-lc|\/c)\s+([\s\S]+)$/i.exec(command);
  if (!match) return command.trim();
  const inner = match[1].trim(), quote = inner[0];
  return (quote === "'" || quote === '"') && inner.endsWith(quote) && inner.length > 1 ? inner.slice(1,-1) : inner;
}

const EASE = 'cubic-bezier(0.23, 1, 0.32, 1)';

// ---- Loader ------------------------------------------------------------------

/** A chevron wavefront driving right across a 3×3 grid. */
const CHEVRON = Array.from({ length: 9 }, (_, i) => {
  const row = Math.floor(i / 3);
  const col = i % 3;
  return (col + Math.abs(row - 1)) * 90;
});

export function LoaderGrid({ size = 4, round = false }: { size?: number; round?: boolean }) {
  return (
    <span
      aria-hidden
      className="ai-loader"
      style={{ gridTemplateColumns: `repeat(3, ${size}px)`, gap: size * 0.375 }}
    >
      {CHEVRON.map((delay, index) => (
        <span
          key={index}
          className={cn('ai-pixel', round ? 'ai-round' : 'ai-square')}
          style={{
            width: size,
            height: size,
            opacity: 0.15,
            animation: `pixel-on 650ms ease-in-out ${delay}ms infinite`,
          }}
        />
      ))}
    </span>
  );
}

/** Text with a light sweeping across it while work is in progress. */
export function ShimmerText({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <span
      className={cn('ai-shimmer', className)}
      style={{
        backgroundImage:
          'linear-gradient(90deg, var(--am-faint) 35%, var(--vscode-foreground) 50%, var(--am-faint) 65%)',
        backgroundSize: '200% 100%',
        animation: 'shimmer-text 1.4s linear infinite',
      }}
    >
      {children}
    </span>
  );
}

function formatElapsed(seconds: number) {
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  return `${Math.floor(seconds / 60)}m ${Math.floor(seconds % 60)}s`;
}

/** Live time since `since`, in tabular mono figures. */
export function Elapsed({ since }: { since: number }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 100);
    return () => clearInterval(timer);
  }, []);
  return (
    <span className="ai-elapsed" aria-hidden="true">
      {formatElapsed(Math.max(0, (now - since) / 1000))}
    </span>
  );
}

/** The status line shown while an agent works: loader, label, elapsed time. */
export function LoadingState({ label, since }: { label: string; since: number }) {
  return (
    <div role="status" className="ai-loading">
      <LoaderGrid />
      <ShimmerText className="ai-loading-label">{label}</ShimmerText>
      <Elapsed since={since} />
    </div>
  );
}

// ---- Tool run ------------------------------------------------------------------

const ICONS: Record<string, ReactNode> = {
  think: <path d="M12 2l2.4 7.2L22 12l-7.6 2.8L12 22l-2.4-7.2L2 12l7.6-2.8z" />,
  write: <path d="M17 3a2.8 2.8 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5z" />,
  run: <path d="M4 17l6-5-6-5M12 19h8" />,
  read: (
    <>
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <path d="M14 2v6h6" />
    </>
  ),
  search: (
    <>
      <circle cx="11" cy="11" r="7" />
      <path d="M21 21l-4.3-4.3" />
    </>
  ),
};

type Step = {
  id: string;
  callId: string | null;
  done: boolean;
  icon: keyof typeof ICONS;
  label: string;
  chip: string | null;
  mono: boolean;
  detail: string[];
  failed: boolean;
};

type FileChip = { path: string; change: 'added' | 'deleted' | 'modified' };

function toolKind(name: string): { icon: keyof typeof ICONS; label: string } {
  const n = name.toLowerCase();
  if (/read|view|open|cat\b|get-content/.test(n)) return { icon: 'read', label: 'Read' };
  if (/edit|write|patch|create|replace|multiedit/.test(n)) return { icon: 'write', label: 'Edit' };
  if (/grep|glob|search|find|list|ls\b/.test(n)) return { icon: 'search', label: 'Search' };
  if (/think|reason|plan|todo/.test(n)) return { icon: 'think', label: name };
  return { icon: 'run', label: /command|bash|shell|exec|run/.test(n) ? 'Run' : name };
}

function inputTarget(input: Record<string, unknown>) {
  const raw = Array.isArray(input.command)
    ? input.command.join(' ')
    : typeof input.command === 'string'
      ? input.command
      : null;
  if (raw) return { text: shellCommand(raw), mono: true };
  for (const key of ['file_path', 'path', 'pattern', 'url', 'query', 'description']) {
    const value = input[key];
    if (typeof value === 'string' && value.trim())
      return { text: value, mono: key !== 'description' };
  }
  return null;
}

/** Pairs each tool call with the result that follows it, and collects file edits. */
export function toolRun(events: AgentThreadEvent[]) {
  const steps: Step[] = [];
  const files = new Map<string, FileChip>();
  for (const event of events) {
    const data = (event.data ?? {}) as Record<string, unknown>;
    if (event.kind === 'file_changed') {
      const text = event.text ?? '';
      const space = text.indexOf(' ');
      const path = space > 0 ? text.slice(space + 1) : text;
      const verb = text.slice(0, Math.max(space, 0)).toLowerCase();
      files.set(path, {
        path,
        change: /add|creat|new/.test(verb)
          ? 'added'
          : /delet|remov/.test(verb)
            ? 'deleted'
            : 'modified',
      });
      continue;
    }
    if (event.kind === 'tool_result') {
      // Pair by the provider's call id; older events without one fall back
      // to the earliest call still waiting for a result.
      const callId = typeof data.call_id === 'string' ? data.call_id : null;
      const step = callId ? steps.find((s) => s.callId === callId) : steps.find((s) => !s.done);
      if (step) {
        const summary = String(data.summary ?? event.text ?? '').trim();
        step.detail = summary ? summary.split(/\r?\n/).filter(Boolean).slice(0, 8) : [];
        step.failed = data.ok === false;
        step.done = true;
      }
      continue;
    }
    if (event.kind !== 'tool_call') continue;
    const { icon, label } = toolKind(event.text || 'Tool');
    const target = inputTarget((data.input ?? {}) as Record<string, unknown>);
    steps.push({
      id: event.id,
      callId: typeof data.call_id === 'string' ? data.call_id : null,
      done: false,
      icon,
      label,
      chip: target?.text ?? null,
      mono: target?.mono ?? false,
      detail: [],
      failed: false,
    });
  }
  return { steps, files: [...files.values()] };
}

function Chevron({ open, size = 12 }: { open: boolean; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="transition-transform duration-200"
      style={{ transform: open ? 'rotate(0deg)' : 'rotate(-90deg)' }}
    >
      <path d="M6 9l6 6 6-6" />
    </svg>
  );
}

function Collapse({ open, children }: { open: boolean; children: ReactNode }) {
  return (
    <div
      className="ai-collapse"
      style={{
        gridTemplateRows: open ? '1fr' : '0fr',
        opacity: open ? 1 : 0,
        transitionTimingFunction: EASE,
      }}
    >
      <div className="ai-collapse-inner" aria-hidden={!open} {...(!open ? { inert: "" } : {})}>{children}</div>
    </div>
  );
}

function ToolRow({ step }: { step: Step }) {
  const [open, setOpen] = useState(false);
  const expandable = step.detail.length > 0;
  return (
    <div style={{ animation: `fade-up 300ms ${EASE} both` }}>
      <button
        type="button"
        aria-expanded={open}
        disabled={!expandable}
        onClick={() => setOpen((v) => !v)}
        className="ai-tool-row"
      >
        <span
          className={cn(
            'ai-tool-icon',
            step.failed ? 'ai-danger' : 'ai-faint',
          )}
        >
          <svg
            width="13"
            height="13"
            viewBox="0 0 24 24"
            fill={step.icon === 'think' ? 'currentColor' : 'none'}
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            className={cn(
              'ai-icon-glyph',
              expandable && 'ai-expandable-glyph',
              open && 'ai-hidden',
            )}
          >
            {ICONS[step.icon]}
          </svg>
          {expandable && (
            <span
              className={cn(
                'ai-tool-chevron',
                open ? 'ai-visible' : 'ai-hidden',
              )}
            >
              <Chevron open={open} />
            </span>
          )}
        </span>
        <span className="ai-tool-label">{step.label}</span>
        {step.chip && (
          <span
            title={step.chip}
            className={cn(
              'ai-tool-target',
              step.mono && 'ai-mono',
            )}
          >
            <span className="ai-truncate">{step.chip}</span>
          </span>
        )}
        {step.failed && <span className="ai-tool-failure">Failed</span>}
      </button>
      <Collapse open={open}>
        <div className="ai-tool-output">
          {step.detail.map((line, index) => (
            <span
              key={index}
              className={cn(
                'ai-output-line',
                step.failed ? 'ai-danger' : 'ai-muted',
              )}
            >
              {line}
            </span>
          ))}
        </div>
      </Collapse>
    </div>
  );
}

/**
 * One stretch of agent work between messages: an expandable list of tool
 * calls and chips for the files it changed. While live, the header shimmers.
 */
export function ToolRun({
  events,
  live,
  onOpenFiles,
}: {
  events: AgentThreadEvent[];
  live: boolean;
  onOpenFiles?: () => void;
}) {
  const { steps, files } = toolRun(events);
  const [manual, setManual] = useState<boolean | null>(null);
  const open = manual ?? live;
  const calls = steps.length;
  const summary = [
    calls > 0 && `Ran ${calls} ${calls === 1 ? 'tool' : 'tools'}`,
    files.length > 0 && `changed ${files.length} ${files.length === 1 ? 'file' : 'files'}`,
  ]
    .filter(Boolean)
    .join(', ');
  const current = steps[steps.length - 1];
  const activeLabel =
    current?.icon === 'read'
      ? 'Reading'
      : current?.icon === 'write'
        ? 'Editing'
        : current?.icon === 'search'
          ? 'Searching'
          : current?.icon === 'run'
            ? 'Running a command'
            : 'Working';
  return (
    <div className="ai-tool-run">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setManual(!open)}
        className="ai-run-heading"
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill={live ? 'var(--am-muted)' : 'var(--am-faint)'}
        >
          <path d="M12 2l2.4 7.2L22 12l-7.6 2.8L12 22l-2.4-7.2L2 12l7.6-2.8z" />
        </svg>
        {live ? (
          <ShimmerText className="ai-run-label">{activeLabel}</ShimmerText>
        ) : (
          <span
            className="ai-run-summary"
            style={{ animation: 'fade-in 350ms ease-out both' }}
          >
            {summary || 'Worked'}
          </span>
        )}
        {steps.some((s) => s.failed) && !live && (
          <span className="ai-tool-failure">· some steps failed</span>
        )}
        <span className="ai-faint">
          <Chevron open={open} />
        </span>
      </button>
      <Collapse open={open}>
        <div className="ai-tool-tree">
          <span aria-hidden className="ai-tree-line" />
          <div className="ai-tool-steps">
            {steps.map((step) => (
              <ToolRow key={step.id} step={step} />
            ))}
          </div>
        </div>
      </Collapse>
      {files.length > 0 && (
        <div className="ai-file-chips">
          {files.map((file, index) => {
            const slash = Math.max(file.path.lastIndexOf('/'), file.path.lastIndexOf('\\'));
            return (
              <button
                key={file.path}
                type="button"
                title={`${file.path} · open Changes`}
                onClick={onOpenFiles}
                className="ai-file-chip"
                style={{ animation: `bui-pop-in 250ms ${EASE} ${index * 60}ms both` }}
              >
                <span className="ai-truncate">{file.path.slice(slash + 1)}</span>
                <span
                  className={cn(
                    'ai-file-change',
                    file.change === 'added'
                      ? 'ai-success'
                      : file.change === 'deleted'
                        ? 'ai-danger'
                        : 'ai-faint',
                  )}
                >
                  {file.change === 'added'
                    ? 'new'
                    : file.change === 'deleted'
                      ? 'deleted'
                      : 'edited'}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

