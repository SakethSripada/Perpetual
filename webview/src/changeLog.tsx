/** File chips and unified diff presentation adapted from Beautiful UI (MIT). */
import type { AgentThreadDiff } from "./types";
import { Icon } from "./icons";

export type DiffLine = { old: number | null; current: number | null; kind: "context" | "add" | "del" | "hunk"; text: string };
export function parsePatch(patch: string): Map<string, DiffLine[]> {
  const files = new Map<string, DiffLine[]>();
  let path = "", previous = "", old = 0, current = 0, inHunk = false;
  const filename = (value: string) => {
    let decoded = value.split("\t")[0];
    if (decoded.startsWith('"')) { try { decoded = JSON.parse(decoded); } catch { /* retain original escaped name */ } }
    return decoded.replace(/^[ab]\//, "");
  };
  for (const line of patch.split(/\r?\n/)) {
    if (line.startsWith("diff --git ")) { inHunk = false; path = ""; previous = ""; continue; }
    if (!inHunk && line.startsWith("--- ")) { previous = filename(line.slice(4)); continue; }
    if (!inHunk && line.startsWith("+++ ")) {
      path = filename(line.slice(4));
      if (path === "/dev/null") path = previous;
      files.set(path, []); continue;
    }
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
    if (hunk && path) {
      old = Number(hunk[1]); current = Number(hunk[2]); inHunk = true;
      files.get(path)!.push({ old: null, current: null, kind: "hunk", text: line }); continue;
    }
    if (!inHunk || !path) continue;
    const rows = files.get(path)!;
    if (line.startsWith("+")) rows.push({ old: null, current: current++, kind: "add", text: line.slice(1) });
    else if (line.startsWith("-")) rows.push({ old: old++, current: null, kind: "del", text: line.slice(1) });
    else if (line.startsWith(" ")) rows.push({ old: old++, current: current++, kind: "context", text: line.slice(1) });
  }
  return files;
}

export function ChangeLog({ diff }: { diff: AgentThreadDiff }) {
  return <div className="change-log">{diff.repos.map((repo) => {
    const patches = parsePatch(repo.patch);
    return <div key={repo.repo_id}>
      {diff.repos.length > 1 && <div className="change-repo">{repo.repo_name}</div>}
      {repo.files.map((file) => <details className="change-file" key={file.path}>
        <summary><Icon name="caret" /><Icon name="file" /><span title={file.path}>{file.path}</span><span className="diff-add">+{file.additions}</span><span className="diff-del">−{file.deletions}</span></summary>
        <div className="change-code" role="region" aria-label={`Changes in ${file.path}`}>
          {patches.get(file.path)?.length ? patches.get(file.path)!.map((line, index) => <div key={index} className={`change-line ${line.kind}`}><span className="change-number" aria-hidden>{line.kind === "del" ? line.old : line.current}</span><span className="change-sign" aria-hidden>{line.kind === "add" ? "+" : line.kind === "del" ? "−" : ""}</span><code>{line.text || " "}</code></div>) : <div className="change-unavailable">{file.additions || file.deletions ? "Preview unavailable" : "File metadata changed"}</div>}
        </div>
      </details>)}
    </div>;
  })}</div>;
}
