use std::time::Duration;

use am_agents::{NormalizedEvent, PermissionPolicy, SessionRuntime, SessionSpec};
use am_proto::{
    new_id, now, AgentKind, EvaluationFollowUp, EvaluationVerdict, EvaluatorPolicy, NewWorkEdge,
    NewWorkNode, WorkEdgeKind, WorkGateEvaluation, WorkNode, WorkPlanRun,
};
use serde::Deserialize;
use serde_json::json;
use tokio::time;

use crate::{AppCore, CoreError};

const EVALUATOR_POLICY_KEY: &str = "evaluator_policy";
const MAX_GATE_CONTEXT_CHARS: usize = 36_000;
const MAX_EVALUATOR_OUTPUT_CHARS: usize = 24_000;

impl AppCore {
    pub async fn get_evaluator_policy(&self) -> Result<EvaluatorPolicy, CoreError> {
        let raw = am_db::repos::settings::get(&self.db.pool, EVALUATOR_POLICY_KEY).await?;
        Ok(raw
            .and_then(|value| serde_json::from_str::<EvaluatorPolicy>(&value).ok())
            .unwrap_or_default())
    }

    pub async fn set_evaluator_policy(
        &self,
        policy: EvaluatorPolicy,
    ) -> Result<EvaluatorPolicy, CoreError> {
        let policy = normalize_evaluator_policy(policy);
        let raw = serde_json::to_string(&policy).unwrap_or_default();
        am_db::repos::settings::set(&self.db.pool, EVALUATOR_POLICY_KEY, &raw).await?;
        self.activity(
            None,
            None,
            "evaluator_policy.updated",
            json!({
                "enabled": policy.enabled,
                "agent": policy.agent.map(|agent| agent.as_str()),
                "create_follow_up_nodes": policy.create_follow_up_nodes,
            }),
        )
        .await?;
        Ok(policy)
    }

    pub async fn list_work_gate_evaluations(
        &self,
        node_id: &str,
    ) -> Result<Vec<WorkGateEvaluation>, CoreError> {
        Ok(am_db::repos::work_graph::list_gate_evaluations(&self.db.pool, node_id).await?)
    }

    pub(crate) async fn evaluate_work_gate(
        &self,
        plan: &WorkPlanRun,
        node: &WorkNode,
        fallback_agent: AgentKind,
    ) -> Result<WorkGateEvaluation, CoreError> {
        let policy = self.get_evaluator_policy().await.unwrap_or_default();
        if !policy.enabled {
            return self
                .store_gate_evaluation(fallback_needs_human(
                    plan,
                    node,
                    None,
                    "Evaluator is disabled in settings.",
                ))
                .await;
        }

        let evaluator_agent = policy.agent.unwrap_or(fallback_agent);
        let context = self.build_gate_evaluation_context(node).await?;
        let prompt = build_gate_evaluator_prompt(&context);
        let output = match self
            .run_evaluator_agent(evaluator_agent, &policy, prompt)
            .await
        {
            Ok(output) => output,
            Err(err) => {
                return self
                    .store_gate_evaluation(fallback_needs_human(
                        plan,
                        node,
                        Some(evaluator_agent),
                        &format!("Evaluator could not run: {err}"),
                    ))
                    .await;
            }
        };

        let mut evaluation = parse_evaluator_output(plan, node, Some(evaluator_agent), &output)
            .unwrap_or_else(|err| {
                fallback_needs_human(
                    plan,
                    node,
                    Some(evaluator_agent),
                    &format!("Evaluator returned invalid JSON: {err}"),
                )
            });
        if evaluation.raw_output.is_empty() {
            evaluation.raw_output = truncate(&output, MAX_EVALUATOR_OUTPUT_CHARS);
        }
        let evaluation = self.store_gate_evaluation(evaluation).await?;

        if evaluation.verdict == EvaluationVerdict::Fail
            && policy.create_follow_up_nodes
            && !evaluation.required_follow_ups.is_empty()
        {
            self.create_evaluation_followups(node, &evaluation).await?;
        }

        Ok(evaluation)
    }

    async fn store_gate_evaluation(
        &self,
        evaluation: WorkGateEvaluation,
    ) -> Result<WorkGateEvaluation, CoreError> {
        let saved =
            am_db::repos::work_graph::insert_gate_evaluation(&self.db.pool, &evaluation).await?;
        self.activity(
            None,
            None,
            "work.gate_evaluated",
            json!({
                "node_id": saved.node_id,
                "plan_run_id": saved.plan_run_id,
                "verdict": saved.verdict.as_str(),
                "confidence": saved.confidence,
            }),
        )
        .await?;
        Ok(saved)
    }

    async fn run_evaluator_agent(
        &self,
        agent: AgentKind,
        policy: &EvaluatorPolicy,
        prompt: String,
    ) -> Result<String, CoreError> {
        self.ensure_session_capacity().await?;
        let adapter = self.agents.get(agent).ok_or_else(|| {
            CoreError::Other(format!("no adapter available for {}", agent.label()))
        })?;
        let status = adapter.detect().await;
        if !status.installed || !status.authenticated {
            return Err(CoreError::Other(format!(
                "{} is not installed and authenticated",
                agent.label()
            )));
        }

        let dir = self
            .data_dir
            .join("evaluations")
            .join(format!("gate-{}", new_id()));
        tokio::fs::create_dir_all(&dir).await.map_err(|err| {
            CoreError::Other(format!("failed to create evaluator workspace: {err}"))
        })?;
        let spec = SessionSpec {
            worktree: dir,
            prompt,
            model: policy.model.clone(),
            reasoning: policy.reasoning.clone(),
            local_model: None,
            mcp: None,
            permission: PermissionPolicy::ReadOnly,
            runtime: SessionRuntime::default(),
            policy: None,
            approver: None,
        };
        let mut handle = adapter
            .start(spec)
            .await
            .map_err(|err| CoreError::Other(err.to_string()))?;
        let timeout = Duration::from_secs(policy.timeout_secs.clamp(30, 1_800));
        let mut output = String::new();
        let result = time::timeout(timeout, async {
            while let Some(event) = handle.events.recv().await {
                match event {
                    NormalizedEvent::AssistantText { text } => {
                        output.push_str(&text);
                        output.push('\n');
                    }
                    NormalizedEvent::Error { message, .. } => {
                        output.push_str("\nEvaluator error: ");
                        output.push_str(&message);
                    }
                    NormalizedEvent::SessionEnded { .. } => break,
                    _ => {}
                }
            }
        })
        .await;
        handle.control.cancel();
        match result {
            Ok(()) => Ok(output),
            Err(_) => Err(CoreError::Other("evaluator timed out".into())),
        }
    }

    async fn build_gate_evaluation_context(&self, node: &WorkNode) -> Result<String, CoreError> {
        let graph = am_db::repos::work_graph::graph(&self.db.pool, &node.project_id).await?;
        let packet = self.build_context_packet(node).await?;
        let diff = self.work_node_diff(&node.id).await.unwrap_or_default();
        let mut out = String::new();
        out.push_str("# Work Gate Evaluation Context\n\n");
        out.push_str(&format!("Node: {} ({})\n", node.title, node.kind.as_str()));
        out.push_str(&format!("Status: {}\n", node.status.as_str()));
        out.push_str(&format!(
            "Objective: {}\n\n",
            node.description.as_deref().unwrap_or("None recorded.")
        ));

        out.push_str("## Graph Relationships\n");
        for edge in &graph.edges {
            if edge.source_id == node.id || edge.target_id == node.id {
                let source = graph
                    .nodes
                    .iter()
                    .find(|candidate| candidate.id == edge.source_id)
                    .map(|candidate| candidate.title.as_str())
                    .unwrap_or(edge.source_id.as_str());
                let target = graph
                    .nodes
                    .iter()
                    .find(|candidate| candidate.id == edge.target_id)
                    .map(|candidate| candidate.title.as_str())
                    .unwrap_or(edge.target_id.as_str());
                out.push_str(&format!("- {} {} {}\n", source, edge.kind.as_str(), target));
            }
        }

        out.push_str("\n## Context Packet\n");
        out.push_str(&packet.summary);
        out.push('\n');
        for inclusion in packet.inclusions.iter().take(12) {
            out.push_str(&format!(
                "- [{}] {}: {}\n",
                inclusion.source_kind,
                inclusion.title,
                truncate(&inclusion.snippet, 700)
            ));
        }

        out.push_str("\n## Diff Summary\n");
        if let Some(task) = diff.task {
            out.push_str(&format!(
                "Task repo: {:?}\nFiles: {}\n{}\n",
                task.repo_name,
                task.files.len(),
                truncate(&task.patch, 10_000)
            ));
        }
        if let Some(thread) = diff.thread {
            for repo in thread.repos {
                out.push_str(&format!(
                    "Thread repo: {} files: {}\n{}\n",
                    repo.repo_name,
                    repo.files.len(),
                    truncate(&repo.patch, 10_000)
                ));
            }
        }

        if let Some(task_id) = &node.task_id {
            if let Ok(events) = self.list_session_events(task_id).await {
                out.push_str("\n## Recent Transcript\n");
                for event in events.iter().rev().take(20).rev() {
                    if let Some(text) = &event.text {
                        out.push_str(&format!(
                            "- {} {}: {}\n",
                            event.role,
                            event.kind,
                            truncate(text, 900)
                        ));
                    }
                }
            }
        }
        if let Some(thread_id) = &node.thread_id {
            if let Ok(events) = self.list_thread_events(thread_id).await {
                out.push_str("\n## Recent Thread Transcript\n");
                for event in events.iter().rev().take(20).rev() {
                    if let Some(text) = &event.text {
                        out.push_str(&format!(
                            "- {} {}: {}\n",
                            event.role,
                            event.kind,
                            truncate(text, 900)
                        ));
                    }
                }
            }
        }

        Ok(truncate(&out, MAX_GATE_CONTEXT_CHARS))
    }

    async fn create_evaluation_followups(
        &self,
        node: &WorkNode,
        evaluation: &WorkGateEvaluation,
    ) -> Result<(), CoreError> {
        for follow_up in evaluation.required_follow_ups.iter().take(8) {
            let created = self
                .create_work_node(NewWorkNode {
                    project_id: node.project_id.clone(),
                    parent_id: node.parent_id.clone(),
                    kind: Some(am_proto::WorkNodeKind::Task),
                    title: follow_up.title.clone(),
                    description: follow_up.description.clone(),
                    priority: follow_up.priority,
                    primary_agent: node.primary_agent,
                    repo_ids: Vec::new(),
                    position_x: None,
                    position_y: None,
                    ..Default::default()
                })
                .await?;
            let _ = self
                .connect_work_nodes(NewWorkEdge {
                    project_id: node.project_id.clone(),
                    source_id: created.id,
                    target_id: node.id.clone(),
                    kind: WorkEdgeKind::Blocks,
                    label: Some("Evaluator follow-up".into()),
                })
                .await;
        }
        Ok(())
    }
}

fn build_gate_evaluator_prompt(context: &str) -> String {
    format!(
        "You are AgentManager's independent gate evaluator. Inspect the bounded context, \
diffs, transcript snippets, blockers, and milestone requirements. Return only strict JSON \
with this shape: {{\"verdict\":\"pass|fail|needs_human\",\"confidence\":0.0,\"findings\":[\"...\"],\
\"required_follow_ups\":[{{\"title\":\"...\",\"description\":\"...\",\"priority\":\"medium\"}}],\
\"validation_commands\":[\"...\"],\"rationale\":\"...\"}}. Use pass only when the work is \
clearly complete and validated. Use fail with targeted follow-ups when concrete work remains. \
Use needs_human when evidence is insufficient.\n\n{context}"
    )
}

#[derive(Deserialize)]
struct RawVerdict {
    verdict: String,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    findings: Vec<String>,
    #[serde(default)]
    required_follow_ups: Vec<EvaluationFollowUp>,
    #[serde(default)]
    validation_commands: Vec<String>,
    #[serde(default)]
    rationale: String,
}

pub(crate) fn parse_evaluator_output(
    plan: &WorkPlanRun,
    node: &WorkNode,
    evaluator_agent: Option<AgentKind>,
    output: &str,
) -> Result<WorkGateEvaluation, String> {
    let json_text = extract_json_object(output).ok_or_else(|| "missing JSON object".to_string())?;
    let raw: RawVerdict = serde_json::from_str(json_text).map_err(|err| err.to_string())?;
    let verdict = EvaluationVerdict::parse(&raw.verdict)
        .ok_or_else(|| format!("unknown verdict '{}'", raw.verdict))?;
    Ok(WorkGateEvaluation {
        id: new_id(),
        plan_run_id: Some(plan.id.clone()),
        node_id: node.id.clone(),
        evaluator_agent,
        verdict,
        confidence: raw.confidence.clamp(0.0, 1.0),
        findings: raw
            .findings
            .into_iter()
            .map(|s| truncate(&s, 700))
            .collect(),
        required_follow_ups: raw
            .required_follow_ups
            .into_iter()
            .filter(|item| !item.title.trim().is_empty())
            .take(12)
            .collect(),
        validation_commands: raw
            .validation_commands
            .into_iter()
            .map(|s| truncate(&s, 300))
            .take(12)
            .collect(),
        rationale: truncate(&raw.rationale, 2_000),
        raw_output: truncate(output, MAX_EVALUATOR_OUTPUT_CHARS),
        created_at: now(),
    })
}

fn fallback_needs_human(
    plan: &WorkPlanRun,
    node: &WorkNode,
    evaluator_agent: Option<AgentKind>,
    rationale: &str,
) -> WorkGateEvaluation {
    WorkGateEvaluation {
        id: new_id(),
        plan_run_id: Some(plan.id.clone()),
        node_id: node.id.clone(),
        evaluator_agent,
        verdict: EvaluationVerdict::NeedsHuman,
        confidence: 0.0,
        findings: vec![rationale.to_string()],
        required_follow_ups: Vec::new(),
        validation_commands: Vec::new(),
        rationale: rationale.to_string(),
        raw_output: String::new(),
        created_at: now(),
    }
}

fn extract_json_object(output: &str) -> Option<&str> {
    let start = output.find('{')?;
    let end = output.rfind('}')?;
    (end > start).then_some(&output[start..=end])
}

fn normalize_evaluator_policy(mut policy: EvaluatorPolicy) -> EvaluatorPolicy {
    policy.timeout_secs = policy.timeout_secs.clamp(30, 1_800);
    policy.model = policy.model.and_then(|model| {
        let trimmed = model.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    });
    policy.reasoning = policy.reasoning.and_then(|reasoning| {
        let trimmed = reasoning.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    });
    policy
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_proto::{GateMode, TaskPriority, WorkNodeKind, WorkPlanRunState};

    fn plan() -> WorkPlanRun {
        WorkPlanRun {
            id: "plan".into(),
            project_id: "project".into(),
            gate_mode: GateMode::AutoEvaluate,
            state: WorkPlanRunState::Running,
            max_active_runs: 4,
            failure_mode: Default::default(),
            max_node_retries: 0,
            steer_dependents_on_unblock: false,
            default_agent: Some(AgentKind::Codex),
            default_permission: Some("read_only".into()),
            default_execution_backend: None,
            evaluator_policy_json: None,
            resume_after_node_id: None,
            policy_envelope_id: None,
            total_count: 1,
            completed_count: 0,
            active_count: 0,
            blocked_count: 0,
            error: None,
            started_at: now(),
            ended_at: None,
            updated_at: now(),
        }
    }

    fn node() -> WorkNode {
        WorkNode {
            id: "node".into(),
            project_id: "project".into(),
            parent_id: None,
            task_id: None,
            thread_id: None,
            kind: WorkNodeKind::Milestone,
            title: "Gate".into(),
            description: None,
            status: am_proto::TaskStatus::Review,
            priority: TaskPriority::Medium,
            primary_agent: None,
            position_x: 0.0,
            position_y: 0.0,
            width: None,
            height: None,
            position_locked: false,
            sort_order: 0,
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn parses_strict_json_verdict() {
        let output = r#"{"verdict":"fail","confidence":0.7,"findings":["missing test"],"required_follow_ups":[{"title":"Add smoke test","priority":"high"}],"validation_commands":["npm run build"],"rationale":"Need validation."}"#;
        let parsed = parse_evaluator_output(&plan(), &node(), Some(AgentKind::Codex), output)
            .expect("valid verdict");
        assert_eq!(parsed.verdict, EvaluationVerdict::Fail);
        assert_eq!(parsed.required_follow_ups[0].title, "Add smoke test");
    }

    #[test]
    fn rejects_unknown_verdicts() {
        let output = r#"{"verdict":"maybe","confidence":0.7}"#;
        assert!(parse_evaluator_output(&plan(), &node(), None, output).is_err());
    }
}
