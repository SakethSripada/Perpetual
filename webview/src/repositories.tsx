import { useMemo, useState } from "react";
import { createPortal } from "react-dom";
import type { Repo } from "./types";
import { Icon } from "./icons";
import { ActionMenu, MenuItem } from "./controls";
import { ResourceState } from "./loading";
import { useSheetAccessibility } from "./dialogs";

export function RepositoryPicker(props: {
  repos: Repo[]; selected: string[]; state: "ready" | "loading" | "error"; locked: boolean; shared: boolean;
  onSelect(ids: string[]): void; onClose(): void; onLocal(): void; onRemove(id: string): void;
}) {
  const [query, setQuery] = useState("");
  const [disconnect, setDisconnect] = useState<Repo | null>(null);
  useSheetAccessibility(true, "repositories");
  const matches = useMemo(() => props.repos.filter((repo) => !!repo.local_path).filter((repo) => `${repo.name} ${repo.local_path ?? repo.remote_url ?? ""}`.toLowerCase().includes(query.trim().toLowerCase())), [props.repos, query]);
  return createPortal(<div className="sheet-backdrop" onMouseDown={props.onClose}><section className="sheet repository-picker" role="dialog" aria-modal="true" aria-label="Local projects" onMouseDown={(event) => event.stopPropagation()}>
    <header><strong>Local projects</strong><button className="quiet-icon" title="Close" aria-label="Close" onClick={props.onClose}><Icon name="close" /></button></header>
    <div className="repository-search"><Icon name="search" /><input data-initial-focus="true" aria-label="Search projects" placeholder="Search projects" value={query} onChange={(event) => setQuery(event.target.value)} /></div>
    <div className="repository-selection-caption"><span>{props.locked ? (props.shared ? "Managed by the host" : "Attached to this chat") : "Choose folders for this chat"}</span>{!props.locked && props.selected.length > 0 && <button className="text-btn" onClick={() => props.onSelect([])}>Deselect all</button>}</div>
    <div className="repository-list">
      {props.state !== "ready" ? <ResourceState state={props.state} label="repositories" /> : !matches.length ? <div className="repository-empty"><Icon name="folder" /><strong>{query ? "No matching projects" : "Select your first project"}</strong>{!query && <span>Choose a local project folder.</span>}</div> : matches.map((repo) => <div className="repository-choice" key={repo.id}>
        <button className="repository-select" role="checkbox" aria-checked={props.selected.includes(repo.id)} aria-label={repo.name} disabled={props.locked} title={props.locked ? "Start a new chat to change repositories" : undefined} onClick={() => props.onSelect(props.selected.includes(repo.id) ? props.selected.filter((id) => id !== repo.id) : [...props.selected, repo.id])}>
          <span className="repository-symbol"><Icon name="folder" /></span><span className="repository-identity"><strong>{repo.name}</strong><small title={repo.local_path ?? repo.remote_url ?? undefined}>{repo.local_path ?? repo.remote_url ?? repo.default_branch}</small></span><span className={`repository-check${props.selected.includes(repo.id) ? " checked" : ""}`}>{props.selected.includes(repo.id) && <Icon name="check" />}</span>
        </button>
        {!props.locked && <ActionMenu label={`Options for ${repo.name}`} trigger={<button className="quiet-icon" aria-label={`Options for ${repo.name}`}><Icon name="more" /></button>}><MenuItem danger onSelect={() => setDisconnect(repo)}><Icon name="trash" />Disconnect repository</MenuItem></ActionMenu>}
      </div>)}
    </div>
    {disconnect && !props.locked && <div className="repository-confirm" role="alert"><span>Disconnect <strong>{disconnect.name}</strong>? Files stay on disk.</span><div><button className="secondary-btn" onClick={() => setDisconnect(null)}>Cancel</button><button className="secondary-btn danger-text" onClick={() => { props.onRemove(disconnect.id); setDisconnect(null); }}>Disconnect</button></div></div>}
    <footer>{!props.shared && <><button className="secondary-btn" onClick={() => { props.onClose(); props.onLocal(); }}><Icon name="folder" />Add project</button></>}<button className="primary-btn" onClick={props.onClose}>Done</button></footer>
  </section></div>, document.body);
}
