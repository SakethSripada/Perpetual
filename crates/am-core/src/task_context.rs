use std::io::ErrorKind;
use std::path::Path;

use am_agents::SessionStatus;
use am_proto::{now, AgentKind, SessionEvent, Task, TaskContext, TaskContextUpdate, TaskDiff};
use chrono::{DateTime, Utc};
use serde_json::json;

use crate::{AppCore, CoreError};

const TASK_CONTEXT_FILE: &str = "TASK_CONTEXT.md";
const CLAUDE_FILE: &str = "CLAUDE.md";
const AGENTS_FILE: &str = "AGENTS.md";
const MANAGED_START: &str = "<!-- AgentManager task context start -->";
const MANAGED_END: &str = "<!-- AgentManager task context end -->";
const MAX_HANDOFF_PROGRESS_BYTES: usize = 16 * 1024;
const MAX_HANDOFF_TEXT_CHARS: usize = 2_000;
const MAX_CHANGED_FILES: usize = 12;

impl AppCore {
    pub async fn get_task_context(&self, task_id: &str) -> Result<Option<TaskContext>, CoreError> {
        let Some(task) = am_db::repos::task::get(&self.db.pool, task_id).await? else {
            return Ok(None);
        };
        Ok(Some(self.ensure_task_context(&task).await?))
    }

    pub async fn update_task_context(
        &self,
        task_id: &str,
        patch: TaskContextUpdate,
    ) -> Result<TaskContext, CoreError> {
        let task = am_db::repos::task::get(&self.db.pool, task_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        self.ensure_task_context(&task).await?;

        let context = am_db::repos::task_context::update(&self.db.pool, task_id, patch).await?;
        self.render_task_context_for_existing_worktree(task_id, &context)
            .await?;
        self.activity(
            Some(task.project_id),
            Some(task.id),
            "context.updated",
            json!({ "task_id": task_id }),
        )
        .await?;

        Ok(context)
    }

    pub(crate) async fn ensure_task_context(&self, task: &Task) -> Result<TaskContext, CoreError> {
        Ok(am_db::repos::task_context::ensure_for_task(&self.db.pool, task).await?)
    }

    pub(crate) async fn sync_task_context_from_task_update(
        &self,
        before: &Task,
        after: &Task,
    ) -> Result<(), CoreError> {
        let Some(mut context) = am_db::repos::task_context::get(&self.db.pool, &before.id).await?
        else {
            self.ensure_task_context(after).await?;
            return Ok(());
        };

        let mut changed = false;
        if before.title != after.title && context.objective == before.title {
            context.objective = after.title.clone();
            changed = true;
        }

        let before_requirements = before.description.clone().unwrap_or_default();
        let after_requirements = after.description.clone().unwrap_or_default();
        if before_requirements != after_requirements && context.requirements == before_requirements
        {
            context.requirements = after_requirements;
            changed = true;
        }

        if changed {
            am_db::repos::task_context::upsert(&self.db.pool, &context).await?;
        }

        Ok(())
    }

    pub(crate) async fn render_task_context_files(
        &self,
        worktree: &Path,
        context: &TaskContext,
    ) -> Result<(), CoreError> {
        let block = managed_context_block(context);

        write_context_file(&worktree.join(TASK_CONTEXT_FILE), &block, true).await?;
        write_context_file(&worktree.join(CLAUDE_FILE), &block, false).await?;
        write_context_file(&worktree.join(AGENTS_FILE), &block, false).await?;

        Ok(())
    }

    pub(crate) async fn apply_session_handoff(
        &self,
        session_id: &str,
        task_id: &str,
        agent: AgentKind,
        status: SessionStatus,
    ) -> Result<String, CoreError> {
        let task = am_db::repos::task::get(&self.db.pool, task_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        // The summary only needs the newest of each interesting kind — not the
        // whole transcript, whose size grows with session length.
        let events = am_db::repos::message::last_events_for_session(
            &self.db.pool,
            session_id,
            task_id,
            &["assistant_text", "usage_limit", "error"],
            24,
        )
        .await?;
        let (diff, diff_error) = match self.task_diff(task_id).await {
            Ok(diff) => (diff, None),
            Err(err) => (TaskDiff::default(), Some(err.to_string())),
        };

        let summary =
            build_handoff_summary(agent, status, now(), &events, &diff, diff_error.as_deref());
        let next_actions = next_actions_for(status, &events);

        // Lossless archive: the rendered progress below keeps a bounded
        // window, but the full handoff history (and the changed-file list
        // that feeds dependent packets) is preserved here.
        let handoff = am_proto::TaskHandoff {
            id: am_proto::new_id(),
            task_id: task_id.to_string(),
            session_id: session_id.to_string(),
            agent,
            status: status_label(status).to_string(),
            summary: summary.clone(),
            changed_files: diff
                .files
                .iter()
                .take(100)
                .map(|file| file.path.clone())
                .collect(),
            next_actions: next_actions.clone(),
            created_at: now(),
        };
        am_db::repos::task_context::insert_handoff(&self.db.pool, &handoff).await?;

        let mut context = self.ensure_task_context(&task).await?;
        context.progress = append_handoff(&context.progress, &summary);
        context.next_actions = next_actions;
        let context = am_db::repos::task_context::upsert(&self.db.pool, &context).await?;

        self.render_task_context_for_existing_worktree(task_id, &context)
            .await?;

        Ok(summary)
    }

    /// Full handoff history for a task, newest first.
    pub async fn list_task_handoffs(
        &self,
        task_id: &str,
        limit: i64,
    ) -> Result<Vec<am_proto::TaskHandoff>, CoreError> {
        Ok(am_db::repos::task_context::list_handoffs(&self.db.pool, task_id, limit).await?)
    }

    async fn render_task_context_for_existing_worktree(
        &self,
        task_id: &str,
        context: &TaskContext,
    ) -> Result<(), CoreError> {
        if let Some(link) = am_db::repos::task_repo::get_for_task(&self.db.pool, task_id).await? {
            if let Some(worktree) = link.worktree_path {
                let path = Path::new(&worktree);
                if path.exists() {
                    self.render_task_context_files(path, context).await?;
                }
            }
        }
        Ok(())
    }
}

async fn write_context_file(path: &Path, block: &str, replace_all: bool) -> Result<(), CoreError> {
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(CoreError::Other(format!(
                "failed to read {}: {err}",
                path.display()
            )));
        }
    };

    let content = if replace_all {
        block.to_string()
    } else {
        merge_managed_block(&existing, block)
    };

    if existing == content {
        return Ok(());
    }

    tokio::fs::write(path, content)
        .await
        .map_err(|err| CoreError::Other(format!("failed to write {}: {err}", path.display())))?;

    Ok(())
}

fn managed_context_block(context: &TaskContext) -> String {
    format!(
        "{MANAGED_START}\n{}{MANAGED_END}\n",
        render_context(context)
    )
}

fn render_context(context: &TaskContext) -> String {
    let mut out = String::new();
    out.push_str("# AgentManager Task Context\n\n");
    out.push_str("This file is generated by AgentManager from the task context record.\n\n");
    out.push_str(&format!("Task: {}\n", clean_value(&context.task_id)));
    out.push_str(&format!("Updated: {}\n\n", context.updated_at.to_rfc3339()));
    push_section(&mut out, "Objective", &context.objective);
    push_section(&mut out, "Requirements", &context.requirements);
    push_section(&mut out, "Decisions", &context.decisions);
    push_section(&mut out, "Progress", &context.progress);
    push_section(&mut out, "Open Questions", &context.open_questions);
    push_section(&mut out, "Next Actions", &context.next_actions);
    out
}

fn push_section(out: &mut String, title: &str, value: &str) {
    out.push_str("## ");
    out.push_str(title);
    out.push('\n');

    let cleaned = clean_value(value);
    let value = cleaned.trim();
    if value.is_empty() {
        out.push_str("None recorded.");
    } else {
        out.push_str(value);
    }
    out.push_str("\n\n");
}

fn clean_value(value: &str) -> String {
    value
        .replace(MANAGED_START, "[AgentManager task context start]")
        .replace(MANAGED_END, "[AgentManager task context end]")
}

fn merge_managed_block(existing: &str, block: &str) -> String {
    if let Some(start) = existing.find(MANAGED_START) {
        let after_start = start + MANAGED_START.len();
        if let Some(end_offset) = existing[after_start..].find(MANAGED_END) {
            let end = after_start + end_offset + MANAGED_END.len();
            let remainder = existing[end..]
                .strip_prefix("\r\n")
                .or_else(|| existing[end..].strip_prefix('\n'))
                .unwrap_or(&existing[end..]);
            return format!("{}{}{}", &existing[..start], block, remainder);
        }
    }

    if existing.trim().is_empty() {
        block.to_string()
    } else {
        format!("{block}\n{existing}")
    }
}

fn build_handoff_summary(
    agent: AgentKind,
    status: SessionStatus,
    ts: DateTime<Utc>,
    events: &[SessionEvent],
    diff: &TaskDiff,
    diff_error: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "### Handoff {} ({})\n",
        ts.to_rfc3339(),
        status_label(status)
    ));
    out.push_str(&format!("- Agent: {}\n", agent.label()));
    out.push_str(&format!("- Status: {}\n", status_label(status)));

    if diff.files.is_empty() {
        out.push_str("- Changed files: none detected.\n");
    } else {
        out.push_str("- Changed files:\n");
        for file in diff.files.iter().take(MAX_CHANGED_FILES) {
            out.push_str(&format!(
                "  - {} ({}, +{}/-{})\n",
                file.path, file.status, file.additions, file.deletions
            ));
        }
        if diff.files.len() > MAX_CHANGED_FILES {
            out.push_str(&format!(
                "  - ... {} more file(s)\n",
                diff.files.len() - MAX_CHANGED_FILES
            ));
        }
    }

    if let Some(error) = diff_error {
        out.push_str(&format!(
            "- Diff note: {}\n",
            truncate_text(error, MAX_HANDOFF_TEXT_CHARS).replace('\n', " ")
        ));
    }

    if let Some(limit) = latest_event_text(events, "usage_limit") {
        out.push_str(&format!(
            "- Usage limit: {}\n",
            truncate_text(limit, MAX_HANDOFF_TEXT_CHARS).replace('\n', " ")
        ));
    }
    if let Some(error) = latest_event_text(events, "error") {
        out.push_str(&format!(
            "- Last error: {}\n",
            truncate_text(error, MAX_HANDOFF_TEXT_CHARS).replace('\n', " ")
        ));
    }

    out.push('\n');
    match latest_assistant_text(events) {
        Some(text) => {
            out.push_str("Recent assistant output:\n");
            out.push_str(&quote_text(&truncate_text(text, MAX_HANDOFF_TEXT_CHARS)));
        }
        None => out.push_str("Recent assistant output: none captured.\n"),
    }

    out.trim_end().to_string()
}

fn next_actions_for(status: SessionStatus, events: &[SessionEvent]) -> String {
    match status {
        SessionStatus::Completed => {
            "Review the worktree diff, run the relevant validation, and mark the task done or resume if more changes are needed.".into()
        }
        SessionStatus::Interrupted => {
            if latest_event_text(events, "usage_limit").is_some() {
                "Continue after the provider limit resets, or switch to another ready agent using the same worktree and context.".into()
            } else {
                "Resume the task from the same worktree and context.".into()
            }
        }
        SessionStatus::Failed => match latest_event_text(events, "error") {
            Some(error) => format!(
                "Investigate the last failure ({}), then resume from the same worktree and context.",
                truncate_text(error, 300).replace('\n', " ")
            ),
            None => {
                "Inspect the failed session, fix the blocker, then resume from the same worktree and context.".into()
            }
        },
    }
}

fn append_handoff(existing: &str, entry: &str) -> String {
    let combined = if existing.trim().is_empty() {
        entry.trim().to_string()
    } else {
        format!("{}\n\n{}", existing.trim_end(), entry.trim())
    };

    if combined.len() <= MAX_HANDOFF_PROGRESS_BYTES {
        return combined;
    }

    let mut start = combined.len() - MAX_HANDOFF_PROGRESS_BYTES;
    while start < combined.len() && !combined.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "Older handoff entries were trimmed to keep context bounded.\n\n{}",
        combined[start..].trim_start()
    )
}

fn latest_assistant_text(events: &[SessionEvent]) -> Option<&str> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == "assistant_text")
        .and_then(|event| event.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn latest_event_text<'a>(events: &'a [SessionEvent], kind: &str) -> Option<&'a str> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == kind)
        .and_then(|event| event.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn quote_text(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    let mut out = String::new();
    let mut chars = value.chars();
    for _ in 0..max_chars {
        match chars.next() {
            Some(ch) => out.push(ch),
            None => return out,
        }
    }
    if chars.next().is_some() {
        out.push_str("\n[truncated]");
    }
    out
}

fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Completed => "completed",
        SessionStatus::Interrupted => "interrupted",
        SessionStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_proto::FileChange;
    use chrono::{TimeZone, Utc};
    use serde_json::Value;

    fn context() -> TaskContext {
        TaskContext {
            task_id: "task-1".into(),
            objective: "Ship the feature".into(),
            requirements: "Keep it fast.".into(),
            decisions: String::new(),
            progress: "Started.".into(),
            open_questions: String::new(),
            next_actions: "Run tests.".into(),
            updated_at: Utc.with_ymd_and_hms(2026, 6, 24, 9, 30, 0).unwrap(),
        }
    }

    fn event(kind: &str, text: Option<&str>) -> SessionEvent {
        SessionEvent {
            id: format!("event-{kind}"),
            session_id: "session-1".into(),
            task_id: "task-1".into(),
            role: "system".into(),
            kind: kind.into(),
            text: text.map(str::to_string),
            data: Value::Null,
            ts: Utc.with_ymd_and_hms(2026, 6, 24, 9, 30, 0).unwrap(),
        }
    }

    #[test]
    fn render_context_includes_sections_and_empty_fallbacks() {
        let rendered = render_context(&context());

        assert!(rendered.contains("# AgentManager Task Context"));
        assert!(rendered.contains("Task: task-1"));
        assert!(rendered.contains("## Objective\nShip the feature"));
        assert!(rendered.contains("## Decisions\nNone recorded."));
        assert!(rendered.contains("## Next Actions\nRun tests."));
    }

    #[test]
    fn merge_managed_block_prepends_without_clobbering_existing_content() {
        let block = managed_context_block(&context());
        let existing = "# Existing guidance\n\nKeep this.";

        let merged = merge_managed_block(existing, &block);

        assert!(merged.starts_with(MANAGED_START));
        assert!(merged.contains(existing));
    }

    #[test]
    fn merge_managed_block_replaces_prior_managed_content() {
        let block = managed_context_block(&context());
        let existing =
            format!("Intro\n{MANAGED_START}\nold generated content\n{MANAGED_END}\n\nManual notes");

        let merged = merge_managed_block(&existing, &block);

        assert!(merged.starts_with("Intro\n"));
        assert!(!merged.contains("old generated content"));
        assert!(merged.contains("Ship the feature"));
        assert!(merged.ends_with("Manual notes"));
    }

    #[test]
    fn build_handoff_summary_captures_status_files_and_output() {
        let diff = TaskDiff {
            files: vec![FileChange {
                path: "src/lib.rs".into(),
                status: "modified".into(),
                additions: 12,
                deletions: 3,
            }],
            patch: String::new(),
            repo_id: Some("repo-1".into()),
            repo_name: Some("owner/repo".into()),
            remote_url: Some("https://github.com/owner/repo.git".into()),
            branch: Some("am/task-1".into()),
            base_ref: Some("base".into()),
            head_ref: Some("head".into()),
            worktree_path: Some("/tmp/worktree".into()),
        };
        let events = vec![event("assistant_text", Some("Implemented the core flow."))];

        let summary = build_handoff_summary(
            AgentKind::Codex,
            SessionStatus::Completed,
            Utc.with_ymd_and_hms(2026, 6, 24, 9, 30, 0).unwrap(),
            &events,
            &diff,
            None,
        );

        assert!(summary.contains("Agent: Codex"));
        assert!(summary.contains("Status: completed"));
        assert!(summary.contains("src/lib.rs (modified, +12/-3)"));
        assert!(summary.contains("> Implemented the core flow."));
    }

    #[test]
    fn next_actions_for_interrupted_usage_limit_suggests_switch_or_wait() {
        let events = vec![event("usage_limit", Some("Usage limit reached"))];

        let actions = next_actions_for(SessionStatus::Interrupted, &events);

        assert!(actions.contains("provider limit resets"));
        assert!(actions.contains("switch to another ready agent"));
    }

    #[test]
    fn append_handoff_keeps_progress_bounded() {
        let existing = "x".repeat(MAX_HANDOFF_PROGRESS_BYTES + 200);
        let appended = append_handoff(&existing, "latest handoff");

        assert!(appended.len() <= MAX_HANDOFF_PROGRESS_BYTES + 80);
        assert!(appended.contains("Older handoff entries were trimmed"));
        assert!(appended.contains("latest handoff"));
    }
}
