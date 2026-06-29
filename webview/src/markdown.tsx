import { memo } from "react";
import type { ReactNode } from "react";

/**
 * Minimal, dependency-free Markdown renderer that emits React nodes (never raw
 * HTML), so agent output is formatted without any XSS surface. Covers the
 * constructs agents actually emit: fenced/inline code, headings, bold/italic,
 * links, blockquotes, and ordered/unordered lists.
 *
 * Memoized on `text`: parsing is pure, so an unchanged message never re-parses
 * when an unrelated part of the app re-renders (e.g. a snapshot tick). This is
 * what keeps a long transcript cheap while messages stream in.
 */
export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return <>{renderBlocks(text.replace(/\r\n/g, "\n"))}</>;
});

function renderBlocks(source: string): ReactNode[] {
  const lines = source.split("\n");
  const blocks: ReactNode[] = [];
  let i = 0;
  let key = 0;

  while (i < lines.length) {
    const line = lines[i];

    // Fenced code block.
    const fence = line.match(/^\s*```(.*)$/);
    if (fence) {
      const lang = fence[1].trim();
      const body: string[] = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) {
        body.push(lines[i]);
        i++;
      }
      i++; // closing fence
      blocks.push(
        <pre key={key++} className="md-pre" data-lang={lang || undefined}>
          <code>{body.join("\n")}</code>
        </pre>
      );
      continue;
    }

    // Blank line.
    if (!line.trim()) {
      i++;
      continue;
    }

    // Heading.
    const heading = line.match(/^(#{1,6})\s+(.*)$/);
    if (heading) {
      const level = heading[1].length;
      const Tag = (`h${Math.min(level + 2, 6)}` as unknown) as "h3";
      blocks.push(
        <Tag key={key++} className="md-h">
          {renderInline(heading[2])}
        </Tag>
      );
      i++;
      continue;
    }

    // Blockquote.
    if (/^\s*>\s?/.test(line)) {
      const quote: string[] = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
        quote.push(lines[i].replace(/^\s*>\s?/, ""));
        i++;
      }
      blocks.push(
        <blockquote key={key++} className="md-quote">
          {renderBlocks(quote.join("\n"))}
        </blockquote>
      );
      continue;
    }

    // Lists (consecutive list items, ordered or unordered).
    if (/^\s*([-*+]|\d+[.)])\s+/.test(line)) {
      const ordered = /^\s*\d+[.)]\s+/.test(line);
      const items: string[] = [];
      while (i < lines.length && /^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*([-*+]|\d+[.)])\s+/, ""));
        i++;
      }
      const children = items.map((item, index) => (
        <li key={index}>{renderInline(item)}</li>
      ));
      blocks.push(
        ordered ? (
          <ol key={key++} className="md-list">
            {children}
          </ol>
        ) : (
          <ul key={key++} className="md-list">
            {children}
          </ul>
        )
      );
      continue;
    }

    // Horizontal rule.
    if (/^\s*([-*_])\1{2,}\s*$/.test(line)) {
      blocks.push(<hr key={key++} className="md-hr" />);
      i++;
      continue;
    }

    // Paragraph: gather until blank line or a block starter.
    const para: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() &&
      !/^\s*```/.test(lines[i]) &&
      !/^(#{1,6})\s+/.test(lines[i]) &&
      !/^\s*>\s?/.test(lines[i]) &&
      !/^\s*([-*+]|\d+[.)])\s+/.test(lines[i])
    ) {
      para.push(lines[i]);
      i++;
    }
    blocks.push(
      <p key={key++} className="md-p">
        {renderInline(para.join("\n"))}
      </p>
    );
  }

  return blocks;
}

// Inline: code spans, bold, italic, links. Code spans are extracted first so
// their contents are never re-parsed for emphasis.
function renderInline(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  const regex = /`([^`]+)`/g;
  let last = 0;
  let key = 0;
  let match: RegExpExecArray | null;
  while ((match = regex.exec(text))) {
    if (match.index > last) {
      nodes.push(...renderEmphasis(text.slice(last, match.index), () => key++));
    }
    nodes.push(
      <code key={key++} className="md-code">
        {match[1]}
      </code>
    );
    last = regex.lastIndex;
  }
  if (last < text.length) {
    nodes.push(...renderEmphasis(text.slice(last), () => key++));
  }
  return nodes;
}

function renderEmphasis(text: string, nextKey: () => number): ReactNode[] {
  // Links first, then bold, then italic — each splits remaining plain text.
  const out: ReactNode[] = [];
  const linkRe = /\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g;
  let last = 0;
  let match: RegExpExecArray | null;
  while ((match = linkRe.exec(text))) {
    if (match.index > last) out.push(...renderBoldItalic(text.slice(last, match.index), nextKey));
    out.push(
      <a key={nextKey()} href={match[2]} target="_blank" rel="noreferrer">
        {match[1]}
      </a>
    );
    last = linkRe.lastIndex;
  }
  if (last < text.length) out.push(...renderBoldItalic(text.slice(last), nextKey));
  return out;
}

function renderBoldItalic(text: string, nextKey: () => number): ReactNode[] {
  const out: ReactNode[] = [];
  const re = /(\*\*|__)(.+?)\1|(\*|_)(.+?)\3/g;
  let last = 0;
  let match: RegExpExecArray | null;
  while ((match = re.exec(text))) {
    if (match.index > last) out.push(text.slice(last, match.index));
    if (match[2] !== undefined) {
      out.push(<strong key={nextKey()}>{match[2]}</strong>);
    } else {
      out.push(<em key={nextKey()}>{match[4]}</em>);
    }
    last = re.lastIndex;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}
